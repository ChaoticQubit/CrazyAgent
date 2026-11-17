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
