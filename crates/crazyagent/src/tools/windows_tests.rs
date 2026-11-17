use super::*;

/// 2.4.3 W9: on Windows the user's temp folder is one root in every spelling, as %TEMP%
/// gives it (8.3 on hosted runners), plain long and verbatim, so a file directly in it
/// passes the place check, while a place outside temp whose real file lies in it still
/// leads into temp. Input: none. Output: passes or panics.
#[test]
fn a_windows_temp_folder_is_one_root_in_every_spelling_and_only_a_link_into_it_is_refused() {
    let roots = temp_roots();
    let owner = std::env::temp_dir();
    let verbatim = owner.canonicalize().unwrap();
    let plain = crate::sandbox::canonical(&owner).unwrap();
    assert!(
        verbatim.to_str().unwrap().starts_with(r"\\?\"),
        "{verbatim:?}"
    );
    assert!(!plain.to_str().unwrap().starts_with(r"\\?\"), "{plain:?}");
    for spelling in [&owner, &verbatim, &plain] {
        assert!(roots.contains(spelling), "{spelling:?} in {roots:?}");
    }
    let folder = tempfile::tempdir().unwrap();
    let file = crate::sandbox::canonical(folder.path())
        .unwrap()
        .join("tool.exe");
    std::fs::write(&file, "").unwrap();
    let real = file.canonicalize().unwrap();
    for written in [folder.path().join("tool.exe"), file.clone(), real.clone()] {
        assert!(in_temp_of(&written, &roots), "{written:?}");
        assert!(!leads_into_temp(&written, &real, &roots), "{written:?}");
    }
    let places = Places::engine(None, None, None, Some(OsStr::new("")));
    assert_eq!(check_place(&places, &file), Some(Ok(())));
    let outside = Path::new(r"C:\CrazyAgentOutsideTemp\bin\tool.exe");
    assert!(leads_into_temp(outside, &real, &roots));
}

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
    let places = Places::new(vec![real.clone()], None);
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
    std::fs::write(&shim, batch::tests::CODEX).unwrap();

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
    std::fs::write(&tool, batch::tests::CODEX.replace("CALL", "calc & CALL")).unwrap();
    let script = other.join("tool.BAT");
    std::fs::write(&script, "@ECHO off\r\n").unwrap();
    for file in [lone_npx, tool, script] {
        let missing = resolve_entry(&file, &places).unwrap_err();
        let refused = missing.refused.unwrap();
        assert_eq!(refused.why, Refusal::Batch, "{file:?}");
        assert_eq!(refused.file, file.canonicalize().unwrap());
    }
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
    let places = Places::new(vec![real.clone()], None);
    let missing = resolve_entry(&script, &places).unwrap_err();
    assert_eq!(missing.refused.unwrap().why, Refusal::FirstLine);
    for name in ["env.exe", "ENV.EXE", "BusyBox.exe", "nice"] {
        assert!(launches_by_name(name), "{name}");
    }
    for name in ["node.exe", "node", "env.cmd", "exe"] {
        assert!(!launches_by_name(name), "{name}");
    }
}

/// Writes the `.cmd` npm makes for Codex and the script it names into a folder. Input: the
/// folder. Output: the shim and the script's real file.
fn codex_shim(folder: &Path) -> (PathBuf, PathBuf) {
    let package = folder
        .join("node_modules")
        .join("@openai")
        .join("codex")
        .join("bin");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("codex.js"), "#!/usr/bin/env node\n").unwrap();
    let shim = folder.join("codex.cmd");
    std::fs::write(&shim, batch::tests::CODEX).unwrap();
    (shim, package.join("codex.js").canonicalize().unwrap())
}

/// V243-WIN-HARNESS-SEARCH: the harness search tries `<name>.exe` then `<name>.cmd` in each
/// folder in order, so a shim in an earlier folder comes before a program in a later one;
/// npm's extensionless sh file and a `.cmd` that is no npm cmd-shim are never found, and the
/// pin of a shim is the script it names.
#[test]
fn a_windows_search_tries_exe_then_cmd_per_folder_and_pins_a_shim_s_script() {
    let root = tempfile::tempdir().unwrap();
    let root = root.path().canonicalize().unwrap();
    let [first, second, other] = ["first", "second", "other"].map(|name| {
        let folder = root.join(name);
        std::fs::create_dir(&folder).unwrap();
        folder
    });
    let (first_shim, first_script) = codex_shim(&first);
    std::fs::write(first.join("codex"), "#!/bin/sh\n").unwrap();
    let (second_shim, _) = codex_shim(&second);
    let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("where.exe");
    std::fs::copy(&shipped, second.join("codex.exe")).unwrap();
    let unknown = other.join("codex.cmd");
    std::fs::write(&unknown, "@ECHO off\r\ncalc.exe\r\n").unwrap();

    let search =
        crate::harnesses::Search::new(vec![other, first.clone(), second.clone()], None, vec![]);
    assert_eq!(
        search.candidates("codex"),
        vec![first_shim.clone(), second.join("codex.exe"), second_shim]
    );
    assert_eq!(
        crate::paths::executable_file(&first_shim),
        Some(first_script)
    );
    assert_eq!(crate::paths::executable_file(&unknown), None);
    assert_eq!(
        crate::paths::executable_names("codex"),
        ["codex.exe", "codex.cmd"]
    );
    assert_eq!(crate::paths::executable_names("codex.cmd"), ["codex.cmd"]);
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
