use super::*;
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
/// text and whether the task starts registered. Output: the service.
#[cfg(unix)]
fn task(folder: &Path, delete: &str, registered: bool) -> Service {
    use std::os::unix::fs::PermissionsExt;
    let stand_in = folder.join("schtasks");
    let script = format!(
        "#!/bin/sh\nstate=\"$(dirname \"$0\")\"\ncase \"$1\" in\n  /Query) [ -e \"$state/registered\" ] ;;\n  /End) exit 0 ;;\n  /Delete) {delete} ;;\n  *) exit 2 ;;\nesac\n"
    );
    std::fs::write(&stand_in, script).unwrap();
    std::fs::set_permissions(&stand_in, std::fs::Permissions::from_mode(0o755)).unwrap();
    if registered {
        std::fs::write(folder.join("registered"), "").unwrap();
    }
    STAND_INS.with(|stand_ins| *stand_ins.borrow_mut() = vec![(SCHTASKS.to_owned(), stand_in)]);
    let mut windows = test_support::service(Platform::Windows);
    windows.root = folder.join("engine");
    windows.definition = folder.join("crazyagent-engine-task.xml");
    std::fs::write(&windows.definition, "<Task/>").unwrap();
    windows
}

/// Task Scheduler can end the engine task yet refuse to delete it, which leaves it to run at
/// the next login: the uninstall then fails naming the refusal, and the definition stays for
/// a retry.
#[cfg(unix)]
#[test]
fn an_uninstall_whose_task_deletion_fails_reports_it_and_keeps_the_definition() {
    let folder = tempfile::tempdir().unwrap();
    let refused = r#"echo "ERROR: Access is denied." >&2; exit 1"#;
    let windows = task(folder.path(), refused, true);
    let error = windows.uninstall().unwrap_err();
    assert!(error.0.contains("still registered"), "{}", error.0);
    assert!(error.0.contains("Access is denied"), "{}", error.0);
    assert!(windows.definition.is_file());
    assert!(folder.path().join("registered").exists());
}

/// A task the deletion removes, or one already gone while its definition is still written,
/// uninstalls cleanly, the definition removed, and a second uninstall does the same.
#[cfg(unix)]
#[test]
fn an_uninstall_removes_a_deleted_or_already_absent_task_and_repeats_safely() {
    let deleted = tempfile::tempdir().unwrap();
    let windows = task(deleted.path(), r#"rm "$state/registered""#, true);
    windows.uninstall().unwrap();
    assert!(!windows.definition.exists());
    assert!(!deleted.path().join("registered").exists());
    let absent = tempfile::tempdir().unwrap();
    let missing = r#"echo "ERROR: The system cannot find the file specified." >&2; exit 1"#;
    let windows = task(absent.path(), missing, false);
    windows.uninstall().unwrap();
    assert!(!windows.definition.exists());
    windows.uninstall().unwrap();
    assert!(!windows.definition.exists());
}
