#![cfg(target_os = "macos")]

use base64::Engine;
use crazyagent::ide::sandbox::{Grants, profile};
use kernel_sandbox::net::SandboxNetwork;
use kernel_sandbox::policy::{AgentFilesRule, IdeAgentFiles};
use kernel_sandbox::toolchains::ToolHomes;
use std::fs;
use std::os::unix::fs::PermissionsExt;

/// Writes the editor profile this commit builds for one fixed layout to the file
/// `E12_GOLDEN_OUT` names: the layout root, the owner's home and tool homes a later row
/// replaces, then each case's profile in base64. Input: the variable; without it the row
/// does nothing. Output: the file.
#[test]
fn the_base_editor_profile_bytes() {
    let Some(out) = std::env::var_os("E12_GOLDEN_OUT") else {
        return;
    };
    let scratch = tempfile::Builder::new()
        .prefix("ca-e12g-")
        .tempdir_in(fs::canonicalize("/tmp").unwrap())
        .unwrap();
    let base = fs::canonicalize(scratch.path()).unwrap();
    let data = base.join("data");
    let mine = data.join("ide").join("p1");
    let build = data.join("ide").join("builds").join("v1");
    let (home, root, tmp) = (mine.join("server"), base.join("project"), base.join("tmp"));
    for path in [&home, &build, &root, &tmp] {
        fs::create_dir_all(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let grants = Grants {
        data: &data,
        root: &root,
        build: &build,
        home: &home,
        tmp: &tmp,
        writable: &[],
        read_only: &[],
        readable: &[],
        locked: &[],
    };
    let read_only = AgentFilesRule {
        choice: IdeAgentFiles::ReadOnly,
        older_agents: true,
    };
    let cases = [
        (
            "closed",
            SandboxNetwork::Only(Vec::new()),
            Vec::new(),
            AgentFilesRule::default(),
        ),
        (
            "open_kept",
            SandboxNetwork::Open,
            vec![11_434],
            AgentFilesRule::default(),
        ),
        (
            "closed_read_only",
            SandboxNetwork::Only(Vec::new()),
            Vec::new(),
            read_only,
        ),
    ];
    let mut text = format!(
        "root={}\nowner_home={}\ntool_homes={:?}\n",
        base.display(),
        kernel_sandbox::paths::owner_home().unwrap().display(),
        ToolHomes::owner().unwrap()
    );
    for (name, network, kept, rule) in cases {
        let bytes = profile(&grants, 45_731, network, &kept, rule).unwrap();
        assert!(bytes.contains(base.to_str().unwrap()));
        let encoded = base64::engine::general_purpose::STANDARD.encode(bytes.as_bytes());
        text.push_str(&format!("{name}={encoded}\n"));
    }
    fs::write(out, text).unwrap();
}
