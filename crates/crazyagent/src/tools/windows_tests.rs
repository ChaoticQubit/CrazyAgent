use crate::tools::{HOOKS, tools_engine};
use kernel_processes::batch::cmd_shim_real;
use kernel_processes::finding::*;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// 2.4.3 W9: on Windows the user's temp folder is one root in every spelling, as %TEMP%
/// gives it (8.3 on hosted runners), plain long and verbatim, so a file directly in it
/// passes the place check, while a place outside temp whose real file lies in it still
/// leads into temp. Input: none. Output: passes or panics.
#[test]
fn a_windows_temp_folder_is_one_root_in_every_spelling_and_only_a_link_into_it_is_refused() {
    let roots = temp_roots(&HOOKS);
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
        assert!(in_temp_of(&HOOKS, &written, &roots), "{written:?}");
        assert!(
            !leads_into_temp(&HOOKS, &written, &real, &roots),
            "{written:?}"
        );
    }
    let places = tools_engine(None, None, None, Some(OsStr::new("")));
    assert_eq!(check_place(&places, &file), Some(Ok(())));
    let outside = Path::new(r"C:\CrazyAgentOutsideTemp\bin\tool.exe");
    assert!(leads_into_temp(&HOOKS, outside, &real, &roots));
}

/// #333 r1 N6: a shim in npm's layout whose target is itself a batch file pins nothing, so
/// the harness search never takes a batch file for a program, while the same shim naming a
/// script pins that script.
#[test]
fn a_shim_whose_target_is_a_batch_file_pins_nothing() {
    let folder = tempfile::tempdir().unwrap();
    let real = folder.path().canonicalize().unwrap();
    let package = real.join("node_modules").join("x");
    std::fs::create_dir_all(&package).unwrap();
    std::fs::write(package.join("run.cmd"), "@ECHO off\r\n").unwrap();
    std::fs::write(package.join("run.js"), "#!/usr/bin/env node\n").unwrap();
    let codex = r"node_modules\@openai\codex\bin\codex.js";
    let to_batch = real.join("batch.cmd");
    std::fs::write(
        &to_batch,
        kernel_processes::batch::CODEX.replace(codex, r"node_modules\x\run.cmd"),
    )
    .unwrap();
    let to_script = real.join("script.cmd");
    std::fs::write(
        &to_script,
        kernel_processes::batch::CODEX.replace(codex, r"node_modules\x\run.js"),
    )
    .unwrap();
    assert_eq!(cmd_shim_real(&to_batch.canonicalize().unwrap()), None);
    assert_eq!(crate::paths::executable_file(&to_batch), None);
    assert_eq!(
        crate::paths::executable_file(&to_script),
        Some(package.join("run.js").canonicalize().unwrap())
    );
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
    std::fs::write(&shim, kernel_processes::batch::CODEX).unwrap();
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
