use super::run;
use std::ffi::OsString;
use std::path::{Component, PathBuf};

/// An agent's program named relative to its current folder still runs through the Windows
/// lane, which makes it absolute before the spawn gate refuses relative programs. The program
/// here is a copy of the system `cmd.exe`, reached by `..` steps from the current folder up to
/// the drive root (V244-WIN-ROOT-RELATIVE-PROGRAM).
#[test]
fn the_windows_lane_still_runs_a_program_named_relative_to_its_folder() {
    let here = std::env::current_dir().unwrap();
    let root = std::path::absolute("\\").unwrap();
    let folder = tempfile::Builder::new()
        .prefix("crazyagent-lane-")
        .tempdir_in(&root)
        .unwrap();
    let shell = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("cmd.exe");
    std::fs::copy(shell, folder.path().join("probe.exe")).unwrap();
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
    let arguments: Vec<OsString> = vec![
        "--".into(),
        relative.into_os_string(),
        "/d".into(),
        "/c".into(),
        "exit".into(),
        "7".into(),
    ];
    let mut out = Vec::new();
    let code = run(&arguments, &mut out);
    assert_eq!(code, 7, "{}", String::from_utf8_lossy(&out));
}
