use super::*;
use std::process::Command;

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    root
}

fn found(name: &str) -> PathBuf {
    let mut folders: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect())
        .unwrap_or_default();
    if let Some(home) = std::env::var_os("HOME") {
        folders.push(PathBuf::from(home).join(".local/bin"));
    }
    folders.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
    folders
        .into_iter()
        .filter(|dir| dir.is_absolute())
        .map(|dir| dir.join(name))
        .find(|candidate| runnable(candidate))
        .unwrap_or_else(|| panic!("prerequisite missing: {name} on PATH"))
}

fn wheel(dir: &Path, python: &Path) -> (PathBuf, String) {
    let stage = dir.join("stage");
    let info = stage.join("demo_tool-1.0.dist-info");
    std::fs::create_dir_all(stage.join("demo_tool")).unwrap();
    std::fs::create_dir_all(&info).unwrap();
    let files = [
        (
            stage.join("demo_tool/__init__.py"),
            "def main():\n    print(\"demo-tool 1.0\")\n",
        ),
        (
            info.join("METADATA"),
            "Metadata-Version: 2.1\nName: demo-tool\nVersion: 1.0\n",
        ),
        (
            info.join("WHEEL"),
            "Wheel-Version: 1.0\nGenerator: crazyagent-test\nRoot-Is-Purelib: true\nTag: py3-none-any\n",
        ),
        (
            info.join("entry_points.txt"),
            "[console_scripts]\ndemo-tool = demo_tool:main\n",
        ),
        (
            info.join("RECORD"),
            "demo_tool/__init__.py,,\ndemo_tool-1.0.dist-info/METADATA,,\ndemo_tool-1.0.dist-info/WHEEL,,\ndemo_tool-1.0.dist-info/entry_points.txt,,\ndemo_tool-1.0.dist-info/RECORD,,\n",
        ),
    ];
    for (path, text) in files {
        std::fs::write(path, text).unwrap();
    }
    let wheel = dir.join("demo_tool-1.0-py3-none-any.whl");
    let status = crate::spawn_gate::status(
        Command::new(python)
            .args(["-I", "-m", "zipfile", "-c"])
            .arg(&wheel)
            .args(["demo_tool", "demo_tool-1.0.dist-info"])
            .current_dir(&stage),
    )
    .unwrap();
    assert!(status.success());
    let sha256 = file_sha256(&wheel).unwrap();
    (wheel, sha256)
}

fn hashed(wheel: &Path, sha256: &str) -> String {
    format!(
        "demo-tool @ file://{} \\\n    --hash=sha256:{sha256}\n",
        wheel.display()
    )
}

fn environments(home: &Path) -> usize {
    std::fs::read_dir(home).map_or(0, |entries| entries.count())
}

#[test]
fn a_locked_tool_installs_only_files_whose_sha256_its_lock_lists() {
    let uv = found("uv");
    let python = found("python3");
    let root = private_root();
    let work = tempfile::tempdir().unwrap();
    let (wheel, sha256) = wheel(work.path(), &python);
    let mut folders = vec![uv.parent().unwrap().to_path_buf()];
    folders.push(python.parent().unwrap().to_path_buf());
    let installer = PinnedInstaller::with_paths(root.path(), folders);
    let good = hashed(&wheel, &sha256);
    let tampered = hashed(&wheel, &"0".repeat(64));
    assert_ne!(good, tampered);
    let unhashed = format!("demo-tool @ file://{}\n", wheel.display());
    let lock = |requirements| ToolLock {
        package: "demo-tool",
        version: "1.0",
        programs: &["demo-tool"],
        requirements,
    };
    let program = root.path().join("setup/bin/demo-tool");
    let home = root.path().join("setup/uv/tools/demo-tool");

    for refused in [tampered.as_str(), unhashed.as_str()] {
        assert_eq!(
            installer.install_locked(&lock(refused), "3"),
            Err(InstallError("setup_install_failed"))
        );
        assert!(std::fs::symlink_metadata(&program).is_err());
        assert_eq!(environments(&home), 0);
    }

    assert_eq!(installer.install_locked(&lock(&good), "3"), Ok(()));
    assert_eq!(installer.program("demo-tool"), Some(program.clone()));
    let report = |installer: &PinnedInstaller| {
        let (ok, output) = run(
            &program,
            &[],
            &installer.env(),
            root.path(),
            Duration::from_secs(30),
        )
        .unwrap();
        assert!(ok);
        String::from_utf8(output).unwrap()
    };
    assert_eq!(report(&installer), "demo-tool 1.0\n");
    let linked = std::fs::read_link(&program).unwrap();
    assert_eq!(environments(&home), 1);

    assert_eq!(installer.install_locked(&lock(&good), "3"), Ok(()));
    assert_eq!(std::fs::read_link(&program).unwrap(), linked);

    assert_eq!(
        installer.install_locked(&lock(&tampered), "3"),
        Err(InstallError("setup_install_failed"))
    );
    assert_eq!(std::fs::read_link(&program).unwrap(), linked);
    assert_eq!(report(&installer), "demo-tool 1.0\n");
    assert_eq!(environments(&home), 1);
}

#[test]
fn a_tool_lock_with_an_unsafe_name_or_python_is_refused_before_uv_runs() {
    let root = private_root();
    let installer = PinnedInstaller::with_paths(root.path(), vec![]);
    let lock = |package, programs| ToolLock {
        package,
        version: "1.0",
        programs,
        requirements: "demo-tool==1.0 \\\n    --hash=sha256:00\n",
    };
    for (package, programs, python) in [
        ("demo-tool", &["demo-tool"][..], "3 --index-url x"),
        ("demo-tool", &["../demo-tool"][..], "3"),
        ("-e", &["demo-tool"][..], "3"),
        ("demo-tool", &[][..], "3"),
    ] {
        assert_eq!(
            installer.install_locked(&lock(package, programs), python),
            Err(InstallError("setup_pin_invalid"))
        );
    }
    assert_eq!(
        installer.install_locked(&lock("demo-tool", &["demo-tool"]), "3"),
        Err(InstallError("setup_uv_missing"))
    );
    assert!(!root.path().join("setup/uv/tools").exists());
}
