use super::stub::*;
use super::*;

use crate::batch::{batch_file, fixed_program};

/// A folder with a stand-in `node.exe` (a copy of the `where.exe` Windows ships, a real
/// program) and places that look there. Input: none. Output: the folder guard, its real
/// spelling and the places.
fn node_folder() -> (tempfile::TempDir, PathBuf, Places) {
    let folder = tempfile::tempdir().unwrap();
    let real = folder.path().canonicalize().unwrap();
    let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("where.exe");
    std::fs::copy(shipped, real.join("node.exe")).unwrap();
    let places = stub_new(vec![real.clone()], None);
    (folder, real, places)
}

/// V243-WIN-HARNESS-SEARCH: the `.cmd` npm wrote for Codex resolves to the script it names,
/// started by the `node.exe` the places hold, never through cmd.exe: the entry stays the
/// `.cmd`, the program and pin is the script's real file, and the words are node then the
/// script.
#[test]
fn an_npm_cmd_shim_runs_as_node_and_the_script_it_names() {
    let (_folder, real, places) = node_folder();
    let package = real
        .join("node_modules")
        .join("@openai")
        .join("codex")
        .join("bin");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("codex.js"), "#!/usr/bin/env node\n").unwrap();
    let shim = real.join("codex.cmd");
    std::fs::write(&shim, crate::batch::CODEX).unwrap();

    let resolved = resolve_entry(&shim, &places).unwrap();
    assert_eq!(resolved.entry, shim);
    assert_eq!(
        resolved.program,
        package.join("codex.js").canonicalize().unwrap()
    );
    assert_eq!(
        resolved.argv(),
        vec![
            real.join("node.exe")
                .canonicalize()
                .unwrap()
                .into_os_string(),
            resolved.program.clone().into_os_string(),
        ]
    );
}

/// npm's own `npx.cmd` runs as the `node.exe` and `npx-cli.js` beside it, whatever its text
/// says; without node beside it, and for any other `.cmd` or `.bat`, the file is refused as
/// a batch file.
#[test]
fn npx_runs_by_its_layout_and_every_other_batch_file_is_refused() {
    let (_folder, real, places) = node_folder();
    let npm = real.join("node_modules").join("npm").join("bin");
    std::fs::create_dir_all(&npm).unwrap();
    std::fs::write(npm.join("npx-cli.js"), "#!/usr/bin/env node\n").unwrap();
    let npx = real.join("npx.cmd");
    std::fs::write(&npx, "@ECHO off\r\ncalc.exe\r\n").unwrap();
    let resolved = resolve_entry(&npx, &places).unwrap();
    assert_eq!(
        resolved.argv(),
        vec![
            real.join("node.exe")
                .canonicalize()
                .unwrap()
                .into_os_string(),
            npm.join("npx-cli.js")
                .canonicalize()
                .unwrap()
                .into_os_string(),
        ]
    );

    let other = tempfile::tempdir().unwrap();
    let other = other.path().canonicalize().unwrap();
    let lone_npx = other.join("npx.cmd");
    std::fs::copy(&npx, &lone_npx).unwrap();
    let tool = other.join("tool.cmd");
    std::fs::write(&tool, crate::batch::CODEX.replace("CALL", "calc & CALL")).unwrap();
    let script = other.join("tool.BAT");
    std::fs::write(&script, "@ECHO off\r\n").unwrap();
    for file in [lone_npx, tool, script] {
        let missing = resolve_entry(&file, &places).unwrap_err();
        let refused = missing.refused.unwrap();
        assert_eq!(refused.why, Refusal::Batch, "{file:?}");
        assert_eq!(refused.file, file.canonicalize().unwrap());
    }
}

/// #341 addendum: npm's `npx.cmd` runs by its layout when its folder is off the engine's
/// PATH, as a portable Node is, since `npx-cli.js` looks its `node` up beside npx.cmd only.
#[test]
fn npx_off_the_engine_s_path_runs_with_the_node_beside_it() {
    let (_folder, real, _places) = node_folder();
    let npm = real.join("node_modules").join("npm").join("bin");
    std::fs::create_dir_all(&npm).unwrap();
    std::fs::write(npm.join("npx-cli.js"), "#!/usr/bin/env node\n").unwrap();
    let npx = real.join("npx.cmd");
    std::fs::write(&npx, "@ECHO off\r\n").unwrap();
    let elsewhere = stub_new(Vec::new(), None);
    let resolved = resolve_entry(&npx, &elsewhere).unwrap();
    let real_of = |file: PathBuf| file.canonicalize().unwrap().into_os_string();
    assert_eq!(
        resolved.argv(),
        vec![
            real_of(real.join("node.exe")),
            real_of(npm.join("npx-cli.js"))
        ]
    );
}

/// #333 r1 sec F1: on Windows a launcher found under another spelling is still a launcher. A
/// first line asking `env` for `Env` finds `env.exe`, which is refused as one, and the name
/// check reads `.exe` and case away, so `ENV.EXE` and `BusyBox.exe` count and `node.exe` not.
#[test]
fn a_launcher_found_under_another_spelling_is_refused_on_windows() {
    let folder = tempfile::tempdir().unwrap();
    let real = folder.path().canonicalize().unwrap();
    let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("where.exe");
    std::fs::copy(shipped, real.join("env.exe")).unwrap();
    let script = real.join("tool.js");
    std::fs::write(&script, "#!/usr/bin/env Env\n").unwrap();
    let places = stub_new(vec![real.clone()], None);
    let missing = resolve_entry(&script, &places).unwrap_err();
    assert_eq!(missing.refused.unwrap().why, Refusal::FirstLine);
    for name in ["env.exe", "ENV.EXE", "BusyBox.exe", "nice"] {
        assert!(launches_by_name(name), "{name}");
    }
    for name in ["node.exe", "node", "env.cmd", "exe"] {
        assert!(!launches_by_name(name), "{name}");
    }
}

/// V243-WIN-HARNESS-SEARCH: no batch file is ever started on Windows. Every spelling Windows
/// reads as a `.cmd` or `.bat` file counts as one, and the spawn gate refuses it before
/// cmd.exe could run it, so a batch file that would leave a marker leaves none.
#[test]
fn no_batch_file_is_ever_started_on_windows() {
    for name in [
        "x.cmd",
        "x.CMD",
        "x.bat",
        "x.Bat",
        "x.cmd.",
        "x.cmd  ",
        "x.cmd::$DATA",
        "x.bat:stream",
    ] {
        assert!(batch_file(Path::new(name)), "{name}");
    }
    for name in ["x.exe", "x.cmdx", "cmd", "x.js", "x.bat.exe"] {
        assert!(!batch_file(Path::new(name)), "{name}");
    }
    let folder = tempfile::tempdir().unwrap();
    let marker = folder.path().join("ran");
    let script = folder.path().join("x.cmd");
    std::fs::write(&script, format!("@echo ran> \"{}\"\r\n", marker.display())).unwrap();
    let refused = crate::spawn_gate::spawn(&mut std::process::Command::new(&script)).unwrap_err();
    assert_eq!(refused.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(!marker.exists());
}

/// A program is fixed only when it is absolute or a bare file name. A root-relative,
/// drive-relative, folder-relative or empty program resolves against a drive or folder the
/// engine did not choose, so it is not (V244-WIN-ROOT-RELATIVE-PROGRAM).
#[test]
fn a_program_is_fixed_only_when_absolute_or_a_bare_file_name() {
    for name in [
        "git",
        "git.exe",
        "C:\\x\\y.exe",
        "C:/x/y.exe",
        "\\\\server\\share\\y.exe",
        "\\\\?\\C:\\y.exe",
    ] {
        assert!(fixed_program(Path::new(name)), "{name}");
    }
    for name in [
        "/usr/bin/git",
        "\\x",
        "/x",
        "C:x",
        "C:",
        "sub\\x",
        "sub/x",
        ".\\x",
        "..\\x",
        ".",
        "..",
        "",
    ] {
        assert!(!fixed_program(Path::new(name)), "{name}");
    }
}

/// On Windows, while the set-up projects cannot be read, a tool in a repository on
/// another drive is agents' to write, and a tool under `ProgramFiles` or `SystemRoot` is not,
/// its verbatim `\\?\` spelling included.
#[test]
fn a_repository_on_another_drive_is_agent_writable_while_its_projects_cannot_be_read() {
    let home = tempfile::tempdir().unwrap();
    let places = Places {
        projects: None,
        ..stub_new(Vec::new(), Some(home.path().canonicalize().unwrap()))
    };
    assert!(places.agent_writable(Path::new(r"D:\repo\bin\tool.exe")));
    let programs = PathBuf::from(std::env::var_os("ProgramFiles").unwrap());
    let kept = programs.join("Git").join("cmd").join("git.exe");
    assert!(!places.agent_writable(&kept), "{}", kept.display());
    let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("where.exe");
    assert!(
        shipped
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .starts_with(r"\\?\")
    );
    assert!(!places.agent_writable(&shipped), "{}", shipped.display());
}

/// A name starting with this drive's letter and `:` joined to a searched folder replaces
/// that folder and reaches the drive's current folder instead, so a real program planted
/// there is not resolved through it: the name is not found, whatever file it would reach.
#[test]
fn a_drive_prefixed_name_never_reaches_a_file_outside_the_searched_folders() {
    let (_folder, _real, places) = node_folder();
    let here = std::env::current_dir().unwrap();
    let drive = here.to_str().unwrap()[..1].to_owned();
    assert!(drive.chars().all(|c| c.is_ascii_alphabetic()), "{drive}");
    let stem = format!("drive-reach-{}", std::process::id());
    let planted = here.join(format!("{stem}.exe"));
    let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("where.exe");
    std::fs::copy(shipped, &planted).unwrap();
    let found = resolve(&format!("{drive}:{stem}"), &places);
    std::fs::remove_file(&planted).unwrap();
    let missing = found.unwrap_err();
    assert_eq!(missing.reason, Reason::NotFound);
    assert!(missing.refused.is_none());
}
