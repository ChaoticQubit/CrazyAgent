#![cfg(target_os = "macos")]

use crazyagent::sandbox::PROFILE_HOOKS;
use kernel_sandbox::editor::{Grants, profile_with};
use kernel_sandbox::net::SandboxNetwork;
use kernel_sandbox::policy::{AgentFilesRule, IdeAgentFiles};
use kernel_sandbox::toolchains::ToolHomes;
use sha2::{Digest, Sha256};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

const ROOT: &str = "/private/tmp/ca-e12-golden";
const FARM_HOME: &str = "/Users/runner";
const PORT: u16 = 45_731;

/// Makes the fixed layout fresh: refuses a linked root, removes what an earlier run left,
/// then creates the data, build, server home, project, tmp and empty owner folders, each
/// 0700. Input: none. Output: the root.
fn fresh_root() -> PathBuf {
    let root = Path::new(ROOT);
    match fs::symlink_metadata(root) {
        Ok(found) => {
            assert!(
                found.is_dir() && !found.file_type().is_symlink(),
                "{ROOT} is not a plain folder"
            );
            fs::remove_dir_all(root).unwrap();
        }
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
    }
    for path in [
        "data/ide/p1/server",
        "data/ide/builds/v1",
        "project",
        "tmp",
        "owner",
    ] {
        let folder = root.join(path);
        fs::create_dir_all(&folder).unwrap();
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    }
    for folder in [root.to_path_buf(), root.join("data"), root.join("data/ide")] {
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700)).unwrap();
    }
    assert_eq!(fs::canonicalize(root).unwrap(), root);
    root.to_path_buf()
}

/// The editor profile is byte-equal to the 2.4.4 one, captured on a macos-15 farm runner
/// before the move for a closed, an open and a read-only agent folders case, over empty
/// tool homes. The profile writes its root raw and also
/// case-folded inside regexes (7 to 10 times a case), so the bytes hold only at exactly
/// `/private/tmp/ca-e12-golden`, which the row removes and recreates for each case; one
/// run at a time per machine. The owner's home appears only raw, as the runner's
/// `/Users/runner`, so it is written as that before the sha256 and the full text compare.
#[test]
fn the_editor_profile_is_byte_equal_to_2_4_4() {
    let owner = kernel_sandbox::paths::owner_home().unwrap();
    let owner = owner.to_str().unwrap();
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
            include_str!("fixtures/editor_profile_golden/closed.sb"),
            "aacdc97ca99d57aee5a87297e92b8ae49f5cc95547bc61097ac29e9a078af98a",
        ),
        (
            "open_kept",
            SandboxNetwork::Open,
            vec![11_434],
            AgentFilesRule::default(),
            include_str!("fixtures/editor_profile_golden/open_kept.sb"),
            "1d0465536e186d1ae3371d06e9262ca0d2ea1766c39b24081a66f1f8170d65c7",
        ),
        (
            "closed_read_only",
            SandboxNetwork::Only(Vec::new()),
            Vec::new(),
            read_only,
            include_str!("fixtures/editor_profile_golden/closed_read_only.sb"),
            "d3aba3177bf479deebad9b007d7cf4bec18af47bd549a0300cd6e9ade8f103df",
        ),
    ];
    for (name, network, kept, rule, golden, sum) in cases {
        let root = fresh_root();
        let (data, home) = (root.join("data"), root.join("data/ide/p1/server"));
        let (build, project, tmp) = (
            root.join("data/ide/builds/v1"),
            root.join("project"),
            root.join("tmp"),
        );
        let grants = Grants {
            data: &data,
            root: &project,
            build: &build,
            home: &home,
            tmp: &tmp,
            writable: &[],
            read_only: &[],
            readable: &[],
            locked: &[],
        };
        let homes = ToolHomes::at(&root.join("owner"));
        let text = profile_with(&PROFILE_HOOKS, &homes, &grants, PORT, network, &kept, rule)
            .unwrap()
            .replace(owner, FARM_HOME);
        let hex: String = Sha256::digest(text.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let first = text
            .bytes()
            .zip(golden.bytes())
            .position(|(made, kept)| made != kept);
        assert!(
            text == golden,
            "{name}: {} bytes against {}, first difference at {first:?}",
            text.len(),
            golden.len()
        );
        assert_eq!(hex, sum, "{name}");
    }
    fs::remove_dir_all(ROOT).unwrap();
}
