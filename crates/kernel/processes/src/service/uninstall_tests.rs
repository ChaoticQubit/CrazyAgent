use super::*;
#[cfg(unix)]
use crate::update::test_support::{TestResult, at};
use std::cell::RefCell;

thread_local! {
    static STAND_INS: RefCell<Vec<(String, PathBuf)>> = const { RefCell::new(Vec::new()) };
}

/// The program a service command runs in rows: the stand-in this thread named for it, else
/// the program itself. Input: the program name. Output: what runs.
pub(super) fn program(program: &str) -> String {
    STAND_INS
        .with(|stand_ins| {
            stand_ins
                .borrow()
                .iter()
                .find(|(name, _)| name == program)
                .map(|(_, path)| path.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| program.to_owned())
}

/// The Windows engine service with its definition in a scratch folder, run on this thread
/// by a stand-in `schtasks` there: `/Query` answers whether the `registered` mark exists,
/// `/End` succeeds and `/Delete` runs the given shell text. Inputs: the folder, the delete
/// text and whether the task starts registered. Output: the service, or the file step that
/// failed.
#[cfg(unix)]
fn task(folder: &Path, delete: &str, registered: bool) -> TestResult<Service> {
    use std::os::unix::fs::PermissionsExt;
    let stand_in = folder.join("schtasks");
    let script = format!(
        "#!/bin/sh\nstate=\"$(dirname \"$0\")\"\ncase \"$1\" in\n  /Query) [ -e \"$state/registered\" ] ;;\n  /End) exit 0 ;;\n  /Delete) {delete} ;;\n  *) exit 2 ;;\nesac\n"
    );
    at(&stand_in, std::fs::write(&stand_in, script))?;
    at(
        &stand_in,
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)),
    )?;
    if registered {
        let mark = folder.join("registered");
        at(&mark, std::fs::write(&mark, ""))?;
    }
    STAND_INS.with(|stand_ins| *stand_ins.borrow_mut() = vec![(SCHTASKS.to_owned(), stand_in)]);
    let mut windows = test_support::service(Platform::Windows);
    windows.root = folder.join("engine");
    windows.definition = folder.join("crazyagent-engine-task.xml");
    at(
        &windows.definition,
        std::fs::write(&windows.definition, "<Task/>"),
    )?;
    Ok(windows)
}

/// Task Scheduler can end the engine task yet refuse to delete it, which leaves it to run at
/// the next login: the uninstall then fails naming the refusal, and the definition stays for
/// a retry.
#[cfg(unix)]
#[test]
fn an_uninstall_whose_task_deletion_fails_reports_it_and_keeps_the_definition() -> TestResult {
    let folder = tempfile::tempdir()?;
    let refused = r#"echo "ERROR: Access is denied." >&2; exit 1"#;
    let windows = task(folder.path(), refused, true)?;
    let error = windows.uninstall().unwrap_err();
    assert!(error.0.contains("still registered"), "{}", error.0);
    assert!(error.0.contains("Access is denied"), "{}", error.0);
    assert!(windows.definition.is_file());
    assert!(folder.path().join("registered").exists());
    Ok(())
}

/// A task the deletion removes, or one already gone while its definition is still written,
/// uninstalls cleanly, the definition removed, and a second uninstall does the same.
#[cfg(unix)]
#[test]
fn an_uninstall_removes_a_deleted_or_already_absent_task_and_repeats_safely() -> TestResult {
    let deleted = tempfile::tempdir()?;
    let windows = task(deleted.path(), r#"rm "$state/registered""#, true)?;
    windows.uninstall().unwrap();
    assert!(!windows.definition.exists());
    assert!(!deleted.path().join("registered").exists());
    let absent = tempfile::tempdir()?;
    let missing = r#"echo "ERROR: The system cannot find the file specified." >&2; exit 1"#;
    let windows = task(absent.path(), missing, false)?;
    windows.uninstall().unwrap();
    assert!(!windows.definition.exists());
    windows.uninstall().unwrap();
    assert!(!windows.definition.exists());
    Ok(())
}

/// Hooks for an install row: the data folder is made, nothing else is asked.
#[cfg(unix)]
const INSTALL_HOOKS: Hooks = Hooks {
    reported_version: |_| None,
    prepare_root: |root| std::fs::create_dir_all(root).map_err(|error| failure(error.to_string())),
    private_folder: |_| Ok(()),
    is_private_folder: |_| true,
};

/// A service of one platform installed from a folder no other user can change, whose
/// manager is a stand-in that logs each call's arguments to `calls` and fails the step
/// named by `refused` with `refused: <step>`. Inputs: the folder, the platform, the manager
/// it stands in for and the refused step. Output: the service, or the file step that failed.
#[cfg(unix)]
fn installing(
    folder: &Path,
    platform: Platform,
    manager: &str,
    refused: &str,
) -> TestResult<Service> {
    use std::os::unix::fs::PermissionsExt;
    let stand_in = folder.join(manager);
    let script = format!(
        "#!/bin/sh\necho \"$*\" >> \"$(dirname \"$0\")/calls\"\ncase \"$*\" in\n  *{refused}*) echo \"refused: {refused}\" >&2; exit 1 ;;\nesac\n[ \"$1\" != /Query ]\n"
    );
    at(&stand_in, std::fs::write(&stand_in, script))?;
    at(
        &stand_in,
        std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)),
    )?;
    STAND_INS.with(|stand_ins| *stand_ins.borrow_mut() = vec![(manager.to_owned(), stand_in)]);
    let mut service = test_support::service(platform);
    service.executable = folder.join("crazyagent");
    at(
        &service.executable,
        std::fs::write(&service.executable, "engine"),
    )?;
    service.root = folder.join("engine");
    service.definition = folder.join("definition");
    Ok(service)
}

/// The calls a stand-in manager logged, one line each. Input: its folder. Output: the lines.
#[cfg(unix)]
fn calls(folder: &Path) -> Vec<String> {
    std::fs::read_to_string(folder.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

/// A registration that fails after the definition is written leaves no definition behind:
/// the unregister steps run, systemd reloads, and the install fails with the manager's
/// refusal, so the next launch installs again instead of starting a unit that never starts
/// at login. A registration that succeeds keeps the definition.
#[cfg(unix)]
#[test]
fn a_failed_registration_removes_the_definition_and_a_registered_one_keeps_it() -> TestResult {
    let folder = test_support::durable_folder();
    let unit = test_support::service(Platform::Linux).name;
    let linux = installing(folder.path(), Platform::Linux, SYSTEMCTL, "enable")?;
    let error = linux.install(&INSTALL_HOOKS).unwrap_err();
    assert!(error.0.contains("refused: enable"), "{}", error.0);
    assert!(!linux.definition.exists());
    assert_eq!(
        calls(folder.path()),
        [
            "--user daemon-reload".to_owned(),
            format!("--user enable --now {unit}"),
            format!("--user disable --now {unit}"),
            "--user daemon-reload".to_owned(),
        ]
    );
    let windows_folder = test_support::durable_folder();
    let windows = installing(
        windows_folder.path(),
        Platform::Windows,
        SCHTASKS,
        "/Create",
    )?;
    let error = windows.install(&INSTALL_HOOKS).unwrap_err();
    assert!(error.0.contains("refused: /Create"), "{}", error.0);
    assert!(!windows.definition.exists());
    let task = &windows.name;
    assert_eq!(
        calls(windows_folder.path())[1..],
        [format!("/End /TN {task}"), format!("/Delete /TN {task} /F")]
    );
    let kept = test_support::durable_folder();
    let linux = installing(kept.path(), Platform::Linux, SYSTEMCTL, "nothing-refused")?;
    linux.install(&INSTALL_HOOKS).unwrap();
    assert!(linux.definition.is_file());
    assert_eq!(
        calls(kept.path()),
        [
            "--user daemon-reload".to_owned(),
            format!("--user enable --now {unit}")
        ]
    );
    Ok(())
}
