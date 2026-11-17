use super::*;

#[cfg(unix)]
const BIG: &str = "/Volumes/Big/Projects";
#[cfg(not(unix))]
const BIG: &str = r"D:\Projects";
#[cfg(target_os = "windows")]
const HOME_HINT: &str = r"Use a full folder path, such as C:\Users\you\Projects.";
#[cfg(target_os = "linux")]
const HOME_HINT: &str = "Use a full folder path, such as /home/you/Projects.";
#[cfg(not(any(target_os = "windows", target_os = "linux")))]
const HOME_HINT: &str = "Use a full folder path, such as /Users/you/Projects.";

#[test]
fn changing_the_home_moves_nothing_unless_move_all_is_chosen() {
    let mut shell = settings(120, 36);
    shell.key(Key::Char('h'));
    find(&render(&mut shell, 120, 36), "/Users/owner/Projects");
    shell.key(Key::Ctrl('u'));
    typed(&mut shell, "Work");
    shell.key(Key::Enter);
    assert_eq!(lead(&shell), HOME_HINT);
    assert!(jobs(&mut shell).is_empty());
    shell.key(Key::Ctrl('u'));
    typed(&mut shell, WORK);
    shell.key(Key::Enter);
    let sent = jobs(&mut shell);
    let [AppSetupJob::FolderSize { path }] = sent.as_slice() else {
        panic!("the current home is measured first")
    };
    assert_eq!(path, "/Users/owner/Projects");
    find(
        &render(&mut shell, 120, 36),
        &format!("Use {WORK} as the projects home?"),
    );
    shell.arrive(event(AppSetupEvent::FolderSize(
        "/Users/owner/Elsewhere".into(),
        Ok(9 * GIB),
    )));
    assert!(
        !render(&mut shell, 120, 36)
            .iter()
            .any(|row| row.contains("9.0 GB")),
        "a size measured for another folder is ignored"
    );
    shell.arrive(event(AppSetupEvent::FolderSize(path.clone(), Ok(3 * GIB))));
    find(
        &render(&mut shell, 120, 36),
        "Move all projects takes 3.0 GB with it.",
    );
    shell.key(Key::Enter);
    let sent = jobs(&mut shell);
    let [
        AppSetupJob::SetHome {
            path,
            move_projects,
            ..
        },
    ] = sent.as_slice()
    else {
        panic!("one home change required")
    };
    assert_eq!((path.as_str(), *move_projects), (WORK, false));
    shell.arrive(event(AppSetupEvent::HomeSet(Ok(()))));
    assert_eq!(
        lead(&shell),
        format!("The projects home is now {WORK}. Nothing was moved.")
    );

    shell.key(Key::Char('h'));
    shell.key(Key::Ctrl('u'));
    typed(&mut shell, BIG);
    shell.key(Key::Enter);
    jobs(&mut shell);
    shell.key(Key::Char('m'));
    let sent = jobs(&mut shell);
    assert!(matches!(
        sent.as_slice(),
        [AppSetupJob::SetHome {
            move_projects: true,
            ..
        }]
    ));
}
