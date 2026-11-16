use super::*;
use crate::setup::pack::{PackManifestEntry, PackSource};
use crate::setup::windows::WindowsInstaller;
use std::path::PathBuf;

const UNSUPPORTED: InstallError = InstallError(PLATFORM_UNSUPPORTED);

struct Case {
    _directory: tempfile::TempDir,
    base: PathBuf,
    links: PathBuf,
}

/// A shipped default-pack entry by id. Input: the id. Output: the entry.
fn shipped(id: &str) -> PackManifestEntry {
    crate::setup::pack::default_pack()
        .unwrap()
        .entries
        .into_iter()
        .find(|entry| entry.id == id)
        .unwrap()
}

/// Writes a program file that passes this system's runnable check. Input: the file.
/// Output: none.
fn program(file: &Path) {
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(file, b"program").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

/// A folder holding the owner's `%LOCALAPPDATA%` with uv in its WinGet Links, as the uv
/// setup installs it. Output: the case, removed when dropped.
fn case() -> Case {
    let directory = tempfile::tempdir().unwrap();
    let base = crate::sandbox::canonical(directory.path()).unwrap();
    let links = base
        .join("local")
        .join("Microsoft")
        .join("WinGet")
        .join("Links");
    program(&links.join("uv.exe"));
    Case {
        _directory: directory,
        base,
        links,
    }
}

/// The Windows installer of a case, with places that hold no program folder. Input: the
/// case. Output: the installer.
fn installer(case: &Case) -> WindowsInstaller {
    WindowsInstaller::new(
        &case.base.join("app"),
        Some(&case.base.join("local")),
        crate::tools::Places::new(Vec::new(), None),
    )
    .unwrap()
}

/// The key a shipped entry's git source is stored under. Input: the entry. Output: the key.
fn git_key(entry: &PackManifestEntry) -> String {
    let PackSource::Git { repo, commit, path } = &entry.source else {
        panic!("{} ships from git", entry.id);
    };
    format!("{repo}@{commit}/{}", path.as_deref().unwrap_or_default())
}

/// WIN-PACK-SEED: a server entry that needs no checkout is set up without one, as Windows
/// seeds it: linear as its HTTP server and serena through the uv the uv setup installs.
#[test]
fn a_server_entry_needing_no_checkout_is_set_up_without_one() {
    let case = case();
    let installer = installer(&case);
    let linear = shipped("linear");
    assert_eq!(
        server_only(&installer, &linear),
        Ok((
            git_key(&linear),
            (
                "linear".to_owned(),
                ServerSpec::Http {
                    url: "https://mcp.linear.app/mcp".into()
                }
            )
        ))
    );
    let serena = shipped("serena");
    let (source, (name, spec)) = server_only(&installer, &serena).unwrap();
    assert_eq!((source, name.as_str()), (git_key(&serena), "serena"));
    let ServerSpec::Stdio { command, args } = spec else {
        panic!("serena runs as a command");
    };
    assert_eq!(command, case.links.join("uv.exe").to_str().unwrap());
    assert_eq!(&args[..3], ["tool", "run", "--from"]);
    assert_eq!(
        &args[4..],
        ["serena", "start-mcp-server", "--project-from-cwd"]
    );
}

/// WIN-PACK-SEED: without a checkout, a skill, a bundled source, a program inside the
/// entry folder and an argument naming that folder are refused as unsupported here, and a
/// server whose tool must install first gets the installer's own refusal.
#[test]
fn what_needs_a_checkout_or_a_tool_is_not_set_up_without_one() {
    let case = case();
    let installer = installer(&case);
    let server = shipped("context-mode");
    let bundled = PackManifestEntry {
        source: PackSource::Bundled {
            path: "context-mode".into(),
        },
        ..server.clone()
    };
    let inside = PackManifestEntry {
        install: "run bin/server as a stdio MCP server".into(),
        ..server.clone()
    };
    let folder = PackManifestEntry {
        install: "run uvx serve --root {dir} as a stdio MCP server".into(),
        ..server.clone()
    };
    for entry in [shipped("i-have-adhd"), bundled, inside, folder] {
        assert_eq!(
            server_only(&installer, &entry),
            Err(UNSUPPORTED),
            "{}",
            entry.install
        );
    }
    assert_eq!(
        server_only(&installer, &shipped("headroom")),
        Err(InstallError("setup_tool_unsupported"))
    );
    assert_eq!(
        pack_server(&installer, &shipped("headroom"), None),
        Err(InstallError("setup_tool_unsupported"))
    );
}

/// WIN-PACK-SEED: on Windows context-mode is set up as the pinned launch through the
/// engine's own `npx.cmd`, the exact spec the context-mode launch check reads and runs
/// through its shared install.
#[cfg(windows)]
#[test]
fn context_mode_is_set_up_on_windows_as_the_pinned_launch_the_engine_runs() {
    let case = case();
    let root = case.base.join("engine");
    let bin = root.join("setup").join("bin");
    program(&bin.join("npx.cmd"));
    let places = crate::tools::Places::from_environment(Some(&root));
    let installer = WindowsInstaller::new(
        &root.join("setup"),
        Some(&case.base.join("local")),
        places.clone(),
    )
    .unwrap();
    let entry = shipped("context-mode");
    let (source, (name, spec)) = server_only(&installer, &entry).unwrap();
    assert_eq!((source, name.as_str()), (git_key(&entry), "context-mode"));
    assert_eq!(
        spec,
        ServerSpec::Stdio {
            command: bin.join("npx.cmd").to_str().unwrap().to_owned(),
            args: vec!["-y".into(), "context-mode@1.0.169".into()],
        }
    );
    assert!(spec.validate().is_ok());
    assert_eq!(
        crate::plugins::context_mode_on(&spec, &places, false),
        Ok(Some((bin.as_path(), "1.0.169")))
    );
}
