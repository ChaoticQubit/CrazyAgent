use super::*;
use crate::protocol::TaskId;
use crate::worker_dispatch::{DispatchLeaseSeal, LeaseIdentity};
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::process::{Command, Stdio};

fn git(root: &Path, args: &[&str]) -> String {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/git")
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .arg("-C")
            .arg(root)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(output.status.success(), "{output:?}");
    String::from_utf8(output.stdout).unwrap()
}

fn pinned(path: &Path) -> LeaseIdentity {
    let metadata = std::fs::metadata(path).unwrap();
    LeaseIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    }
}

fn renumbered(identity: &LeaseIdentity) -> LeaseIdentity {
    LeaseIdentity {
        device: identity.device ^ 1,
        inode: identity.inode,
    }
}

#[test]
fn dispatch_files_accepts_a_uniform_renumber_and_refuses_a_mixed_one() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let repository = root.join("repo");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-b", "main"]);
    std::fs::write(repository.join("tracked"), "committed\n").unwrap();
    git(&repository, &["add", "tracked"]);
    git(
        &repository,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "commit",
            "-m",
            "base",
        ],
    );
    let target = root.join("lease");
    git(
        &repository,
        &[
            "worktree",
            "add",
            "-b",
            "crazyagent/task",
            target.to_str().unwrap(),
        ],
    );
    let admin = repository.join(".git/worktrees/lease");
    let seal = DispatchLeaseSeal {
        project_id: crate::protocol::ProjectId::new("project").unwrap(),
        task_id: TaskId::new("task").unwrap(),
        repository: repository.to_str().unwrap().into(),
        path: target.to_str().unwrap().into(),
        branch: "crazyagent/task".into(),
        owner_session: crate::protocol::SessionId::new("owner").unwrap(),
        native_descendant: None,
        admission_root: pinned(&repository),
        admission_metadata: pinned(&repository.join(".git")),
        directory: pinned(&target),
        metadata: pinned(&admin),
        head: git(&target, &["rev-parse", "HEAD"]).trim().to_owned(),
    };
    assert_eq!(dispatch_files(&seal).unwrap(), admin);

    let uniform = DispatchLeaseSeal {
        admission_root: renumbered(&seal.admission_root),
        admission_metadata: renumbered(&seal.admission_metadata),
        directory: renumbered(&seal.directory),
        metadata: renumbered(&seal.metadata),
        ..seal.clone()
    };
    assert_eq!(dispatch_files(&uniform).unwrap(), admin);

    for moved in 0..4 {
        let mut mixed = seal.clone();
        let pin = [
            &mut mixed.admission_root,
            &mut mixed.admission_metadata,
            &mut mixed.directory,
            &mut mixed.metadata,
        ]
        .into_iter()
        .nth(moved)
        .unwrap();
        *pin = renumbered(pin);
        assert_eq!(
            dispatch_files(&mixed).unwrap_err().0,
            "dispatch_filesystem_identity_changed",
            "only pin {moved} renumbered"
        );
    }
    let replaced = DispatchLeaseSeal {
        directory: LeaseIdentity {
            device: seal.directory.device ^ 1,
            inode: seal.directory.inode + 1,
        },
        ..uniform.clone()
    };
    assert_eq!(
        dispatch_files(&replaced).unwrap_err().0,
        "dispatch_filesystem_identity_changed"
    );
}

#[test]
fn dispatch_reads_a_repository_config_larger_than_64_kib_and_still_refuses_its_keys() {
    use std::fmt::Write;
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
    let repository = root.join("repo");
    std::fs::create_dir(&repository).unwrap();
    git(&repository, &["init", "-b", "main"]);
    let config = repository.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    for branch in 0..1000 {
        let name = format!("feature/scholar-scope-{branch:04}");
        write!(
            text,
            "[branch \"{name}\"]\n\tremote = origin\n\tmerge = refs/heads/{name}\n"
        )
        .unwrap();
    }
    std::fs::write(&config, &text).unwrap();
    assert!(text.len() > 100_000);
    dispatch_config(&repository).unwrap();
    text.push_str("[core]\n\tfsmonitor = /bin/true\n");
    std::fs::write(&config, &text).unwrap();
    assert_eq!(
        dispatch_config(&repository).unwrap_err().0,
        "dispatch_git_config_unsupported"
    );
}
