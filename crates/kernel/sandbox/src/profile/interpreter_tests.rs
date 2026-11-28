use super::ProjectSandbox;
use crate::net::SandboxNetwork;
use crate::paths::canonical;
use crate::test_hooks::STAND_IN;
use crate::toolchains::{ToolHomes, Toolchains};
use kernel_processes::finding::{Kind, Resolved};
use std::path::PathBuf;

/// On Linux a toolchain folder is never granted, so an interpreter inside one still gets a
/// read grant of its folder: only the system folders and granted read folders count as
/// covering it.
#[test]
fn an_interpreter_in_a_toolchain_folder_is_still_granted_on_linux() {
    let scratch = tempfile::tempdir().unwrap();
    let root = canonical(scratch.path()).unwrap();
    let [project, home, tmp, tools] =
        ["project", "home", "tmp", "tools"].map(|name| root.join(name));
    for folder in [&project, &home, &tmp, &tools.join("bin")] {
        std::fs::create_dir_all(folder).unwrap();
    }
    let interpreter = tools.join("bin").join("python3");
    std::fs::write(&interpreter, b"").unwrap();
    let toolchains = Toolchains::grant(&ToolHomes::at(&home).with_xcode(&tools), &[]);
    let sandbox = ProjectSandbox::new(&STAND_IN, &project, &home, &tmp, SandboxNetwork::Open)
        .unwrap()
        .allow_tools(&toolchains)
        .unwrap();
    assert!(
        sandbox.tool_roots().contains(&tools),
        "{:?}",
        sandbox.tool_roots()
    );
    let node = Resolved {
        entry: interpreter.clone(),
        program: interpreter.clone(),
        kind: Kind::Native,
    };
    let resolved = Resolved::script(
        project.join("missing-entry"),
        tmp.join("script"),
        node,
        Vec::new(),
        false,
    )
    .unwrap();
    let granted = sandbox.allow_interpreters(&resolved).unwrap();
    let folder: PathBuf = tools.join("bin");
    assert!(
        granted
            .read_roots()
            .iter()
            .any(|root| folder.starts_with(root)),
        "{:?}",
        granted.read_roots()
    );
}
