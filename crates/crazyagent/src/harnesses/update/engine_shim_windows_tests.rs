use super::*;
use rusqlite::Connection;

/// A stand-in Node, as the engine npm rows make one: the curl Windows ships as `node.exe`,
/// npm's own script below it with the first line npm writes, and `npm.cmd` beside it.
/// Input: the folder, made here. Output: none.
fn stand_in_node(folder: &Path) {
    let curl = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("curl.exe");
    let cli = folder.join("node_modules").join("npm").join("bin");
    std::fs::create_dir_all(&cli).unwrap();
    std::fs::copy(&curl, folder.join("node.exe")).unwrap();
    std::fs::write(cli.join("npm-cli.js"), "#!/usr/bin/env node\n").unwrap();
    std::fs::write(folder.join("npm.cmd"), "@ECHO off\r\n").unwrap();
}

/// An engine at a root whose npm is the one beside the Node given, with its prefix made
/// private as an install into it makes it. Inputs: the engine root and the Node folder.
/// Output: the engine's roots.
fn engine_at(engine_root: &Path, node: &Path) -> Roots {
    let places = crate::tools::tools_new(vec![node.to_path_buf()], None);
    let npm = state::engine_npm_at(engine_root, &places).expect("the stand-in Node");
    for made in [npm.prefix.clone(), npm.prefix.join("node_modules")] {
        crate::paths::private_folder(&made).unwrap();
    }
    Roots {
        home: state::home_folder().unwrap(),
        brew_prefixes: Vec::new(),
        casks: false,
        npm: Some(npm),
        places,
        elevated: false,
    }
}

/// An engine with a stand-in Node, under this user's profile so no temp folder rule
/// applies. Output: the folder kept for the row and the engine's roots.
fn engine() -> (tempfile::TempDir, Roots) {
    let home = state::home_folder().unwrap();
    let folder = tempfile::tempdir_in(&home).unwrap();
    let root = kernel_sandbox::paths::canonical(folder.path()).unwrap();
    stand_in_node(&root.join("node"));
    let roots = engine_at(&root.join("engine"), &root.join("node"));
    (folder, roots)
}

/// Writes the codex npm installs into a prefix: its script below `node_modules` and the
/// cmd-shim text given at the prefix root. Inputs: the prefix and the shim text. Output:
/// the shim.
fn codex_in(prefix: &Path, shim_text: &str) -> PathBuf {
    let bin = prefix
        .join("node_modules")
        .join("@openai")
        .join("codex")
        .join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::fs::write(bin.join("codex.js"), "#!/usr/bin/env node\n").unwrap();
    let shim = prefix.join("codex.cmd");
    std::fs::write(&shim, shim_text).unwrap();
    shim
}

/// The versions row of codex at a shim, with updates off so nothing is started. Inputs:
/// the roots and the shim. Output: the row.
fn versions_at(roots: &Roots, shim: &Path) -> HarnessVersions {
    let db = Connection::open_in_memory().unwrap();
    initialize(&db).unwrap();
    let search = harnesses::Search::new(Vec::new(), None, Vec::new());
    let mut entry = harnesses::catalog(&search)
        .into_iter()
        .find(|entry| entry.id == "codex")
        .unwrap();
    entry.executable = Some(shim.to_path_buf());
    Updates::new().versions(&db, &entry, roots, false)
}

/// The codex npm installs into the engine prefix reads as an npm install of
/// `@openai/codex` with its update command, as on macOS and Linux: its PATH entry on
/// Windows is npm's cmd-shim, which names the script below the prefix's `node_modules`.
#[test]
fn a_windows_npm_harness_in_the_engine_prefix_reads_as_npm_with_its_update_command() {
    let (_folder, roots) = engine();
    let prefix = roots.npm.clone().unwrap().prefix;
    let shim = codex_in(&prefix, kernel_processes::batch::CODEX);
    let read = versions_at(&roots, &shim);
    assert_eq!(
        read.method,
        Method::Npm {
            package: "@openai/codex".into()
        },
        "{read:?}"
    );
    assert_eq!(read.refusal, None, "{read:?}");
    assert!(read.command.is_some(), "{read:?}");
}

/// A cmd-shim is read for the install method only inside the engine prefix and only when
/// its target is the real file below that prefix's `node_modules`: one outside the engine
/// prefix, one naming a file outside `node_modules`, one whose package folder is a link
/// to an npm layout elsewhere, and one another account can change all stay unknown.
#[test]
fn a_windows_shim_outside_the_engine_prefix_or_its_modules_stays_an_unknown_install() {
    let (folder, roots) = engine();
    let prefix = roots.npm.clone().unwrap().prefix;
    let root = kernel_sandbox::paths::canonical(folder.path()).unwrap();
    let unknown = |shim: &Path| {
        let read = versions_at(&roots, shim);
        assert_eq!(read.method, Method::Unknown, "{}: {read:?}", shim.display());
        assert_eq!(
            read.refusal.as_deref(),
            Some(UNKNOWN_INSTALL),
            "{}: {read:?}",
            shim.display()
        );
    };

    let other = root.join("other");
    for made in [other.clone(), other.join("node_modules")] {
        crate::paths::private_folder(&made).unwrap();
    }
    unknown(&codex_in(&other, kernel_processes::batch::CODEX));

    let at_root_roots = engine_at(&root.join("at-root"), &root.join("node"));
    let at_root_npm = at_root_roots.npm.clone().unwrap().prefix;
    let beside = kernel_processes::batch::CODEX.replace(r"node_modules\@openai\codex\bin\", "");
    assert_ne!(beside, kernel_processes::batch::CODEX);
    std::fs::write(at_root_npm.join("codex.js"), "#!/usr/bin/env node\n").unwrap();
    let shim = at_root_npm.join("codex.cmd");
    std::fs::write(&shim, &beside).unwrap();
    let read = versions_at(&at_root_roots, &shim);
    assert_eq!(read.method, Method::Unknown, "{read:?}");
    assert_eq!(read.refusal.as_deref(), Some(UNKNOWN_INSTALL), "{read:?}");

    let elsewhere = root.join("elsewhere");
    codex_in(&elsewhere, kernel_processes::batch::CODEX);
    let scope = prefix.join("node_modules").join("@openai");
    std::fs::create_dir_all(&scope).unwrap();
    std::os::windows::fs::symlink_dir(
        elsewhere.join("node_modules").join("@openai").join("codex"),
        scope.join("codex"),
    )
    .unwrap();
    let shim = prefix.join("codex.cmd");
    std::fs::write(&shim, kernel_processes::batch::CODEX).unwrap();
    unknown(&shim);

    std::fs::remove_dir(scope.join("codex")).unwrap();
    let shim = codex_in(&prefix, kernel_processes::batch::CODEX);
    let granted = kernel_processes::spawn_gate::collected(
        std::process::Command::new("icacls")
            .arg(&shim)
            .args(["/grant", "*S-1-5-32-545:M"]),
    )
    .unwrap();
    assert!(granted.status.success(), "{granted:?}");
    unknown(&shim);
}
