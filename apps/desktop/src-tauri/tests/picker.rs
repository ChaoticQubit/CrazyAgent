#![cfg(unix)]

use crazyagent_desktop::bridge::picker::{
    DOCUMENTS_SCRIPT, FOLDER_SCRIPT, FolderPurpose, PROJECT_FOLDER_SCRIPT, PROJECTS_HOME_SCRIPT,
    Pick, chooser_command, read_reply, run_chooser,
};
use std::process::Command;
use std::time::Duration;

#[test]
fn a_chosen_folder_is_returned_as_one_absolute_path_without_the_trailing_slash() {
    let pick = read_reply(true, b"/Users/owner/skills/frontend-design/\n", b"").unwrap();
    assert_eq!(
        pick,
        Pick::Paths(vec!["/Users/owner/skills/frontend-design".into()])
    );
    let root = read_reply(true, b"/\n", b"").unwrap();
    assert_eq!(root, Pick::Paths(vec!["/".into()]));
}

#[test]
fn several_chosen_documents_come_back_in_order() {
    let reply = b"/Users/owner/Docs/brief.pdf\n/Users/owner/Docs/market notes.md\n";
    let pick = read_reply(true, reply, b"").unwrap();
    assert_eq!(
        pick,
        Pick::Paths(vec![
            "/Users/owner/Docs/brief.pdf".into(),
            "/Users/owner/Docs/market notes.md".into(),
        ])
    );
}

#[test]
fn cancelling_the_chooser_is_not_an_error() {
    let stderr = b"0:41: execution error: User canceled. (-128)\n";
    assert_eq!(read_reply(false, b"", stderr).unwrap(), Pick::Canceled);
    assert_eq!(read_reply(true, b"\n", b"").unwrap(), Pick::Canceled);
}

#[test]
fn a_failed_or_unexpected_reply_is_refused_with_a_plain_code() {
    assert_eq!(
        read_reply(false, b"", b"execution error: boom (-1743)")
            .unwrap_err()
            .code,
        "picker_failed"
    );
    assert_eq!(
        read_reply(true, b"relative/path\n", b"").unwrap_err().code,
        "picker_failed"
    );
    assert_eq!(
        read_reply(true, b"/ok\0bad\n", b"").unwrap_err().code,
        "picker_failed"
    );
    assert_eq!(
        read_reply(true, &[0xff, 0xfe, b'\n'], b"")
            .unwrap_err()
            .code,
        "picker_failed"
    );
    let huge = vec![b'/'; 70 * 1024];
    assert_eq!(
        read_reply(true, &huge, b"").unwrap_err().code,
        "picker_failed"
    );
    let many: String = (0..65).map(|index| format!("/file-{index}\n")).collect();
    assert_eq!(
        read_reply(true, many.as_bytes(), b"").unwrap_err().code,
        "picker_failed"
    );
}

#[test]
fn the_chooser_scripts_are_fixed_text_that_only_ask_for_a_choice() {
    for script in [FOLDER_SCRIPT, DOCUMENTS_SCRIPT] {
        assert!(script.contains("choose "));
        assert!(!script.contains("do shell script"));
        assert!(!script.contains("tell application"));
    }
    assert!(FOLDER_SCRIPT.contains("choose folder"));
    assert!(DOCUMENTS_SCRIPT.contains("with multiple selections allowed"));
}

#[test]
fn each_folder_purpose_asks_for_its_own_folder_with_fixed_text() {
    assert_eq!(FolderPurpose::Skill.script(), FOLDER_SCRIPT);
    assert_eq!(FolderPurpose::Project.script(), PROJECT_FOLDER_SCRIPT);
    assert_eq!(FolderPurpose::ProjectsHome.script(), PROJECTS_HOME_SCRIPT);
    assert!(FOLDER_SCRIPT.contains("SKILL.md"));
    for script in [PROJECT_FOLDER_SCRIPT, PROJECTS_HOME_SCRIPT] {
        assert!(
            !script.contains("skill") && !script.contains("SKILL.md"),
            "adding a project never asks for a skill folder: {script}"
        );
    }
    assert!(PROJECT_FOLDER_SCRIPT.contains("project folder to add"));
    assert!(PROJECTS_HOME_SCRIPT.contains("holds your projects"));
    for (index, purpose) in FolderPurpose::ALL.into_iter().enumerate() {
        let script = purpose.script();
        assert!(script.starts_with("POSIX path of (choose folder with prompt \""));
        assert!(!script.contains("do shell script") && !script.contains("tell application"));
        assert_eq!(script.matches('"').count(), 2, "one fixed prompt: {script}");
        assert!(
            FolderPurpose::ALL[..index]
                .iter()
                .all(|other| other.script() != script),
            "each purpose has its own prompt"
        );
    }
    let named = |text: &str| serde_json::from_str::<FolderPurpose>(text);
    assert_eq!(named("\"skill\"").unwrap(), FolderPurpose::Skill);
    assert_eq!(named("\"project\"").unwrap(), FolderPurpose::Project);
    assert_eq!(
        named("\"projects_home\"").unwrap(),
        FolderPurpose::ProjectsHome
    );
    for unknown in ["\"documents\"", "\"Skill\"", "\"\"", "3", "null"] {
        assert!(named(unknown).is_err(), "{unknown} names no chooser");
    }
}

#[cfg(target_os = "macos")]
#[test]
fn macos_choosers_run_the_fixed_scripts_through_osascript() {
    for script in [FOLDER_SCRIPT, DOCUMENTS_SCRIPT] {
        let command = chooser_command(script).unwrap();
        assert_eq!(command.get_program(), "/usr/bin/osascript");
        assert!(command.get_args().eq(["-e", script]));
    }
    for purpose in FolderPurpose::ALL {
        let command = chooser_command(purpose.script()).unwrap();
        assert!(command.get_args().eq(["-e", purpose.script()]));
    }
}

#[cfg(target_os = "linux")]
#[test]
fn linux_choosers_use_zenity_or_kdialog_from_a_fixed_folder() {
    let installed = ["zenity", "kdialog"].iter().find_map(|name| {
        ["/usr/bin", "/usr/local/bin", "/run/current-system/sw/bin"]
            .iter()
            .map(|dir| std::path::Path::new(dir).join(name))
            .find(|path| path.is_file())
    });
    for script in [FOLDER_SCRIPT, DOCUMENTS_SCRIPT] {
        match (&installed, chooser_command(script)) {
            (Some(tool), Ok(command)) => {
                assert_eq!(command.get_program(), tool.as_os_str());
                let args: Vec<_> = command.get_args().collect();
                let directory = args
                    .iter()
                    .any(|arg| *arg == "--directory" || *arg == "--getexistingdirectory");
                assert_eq!(directory, script == FOLDER_SCRIPT);
            }
            (None, Err(error)) => assert_eq!(error.code, "picker_unavailable"),
            (expected, got) => panic!(
                "installed {expected:?}, chooser {:?}",
                got.map(|c| c.get_program().to_owned())
            ),
        }
    }
    for purpose in FolderPurpose::ALL {
        if let Ok(command) = chooser_command(purpose.script()) {
            let args: Vec<String> = command
                .get_args()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            assert!(
                args.iter()
                    .any(|arg| arg == "--directory" || arg == "--getexistingdirectory")
            );
            let skill = args.iter().any(|arg| arg.contains("SKILL.md"));
            assert_eq!(skill, purpose == FolderPurpose::Skill, "{args:?}");
        }
    }
    let crlf = read_reply(true, b"/home/owner/skills/\r\n", b"").unwrap();
    assert_eq!(crlf, Pick::Paths(vec!["/home/owner/skills".into()]));
}

fn shell(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script, "sh"]);
    command
}

#[tokio::test]
async fn a_chooser_reply_is_read_from_the_real_child_it_starts() {
    let pick = run_chooser(shell(
        "printf '/tmp/owner/brief.pdf\\n/tmp/owner/skills/\\n'",
    ))
    .await
    .unwrap();
    assert_eq!(
        pick,
        Pick::Paths(vec![
            "/tmp/owner/brief.pdf".into(),
            "/tmp/owner/skills".into()
        ])
    );
    let canceled = run_chooser(shell(
        "echo 'execution error: User canceled. (-128)' >&2; exit 1",
    ))
    .await
    .unwrap();
    assert_eq!(canceled, Pick::Canceled);
    let failed = run_chooser(shell("echo 'execution error: boom (-1743)' >&2; exit 2"))
        .await
        .unwrap_err();
    assert_eq!(failed.code, "picker_failed");
}

#[tokio::test]
async fn a_chooser_does_not_start_while_another_thread_holds_the_spawn_gate() {
    let folder = tempfile::tempdir().unwrap();
    let marker = folder.path().join("started");
    let (held, ready) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        crazyagent::spawn_gate::guarded(|| {
            held.send(()).unwrap();
            let _ = wait.recv();
        })
    });
    ready.recv().unwrap();
    let mut command = shell(": > \"$1\"");
    command.arg(&marker);
    let pick = tokio::spawn(run_chooser(command));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let started_while_held = marker.exists();
    release.send(()).unwrap();
    holder.join().unwrap();
    assert!(
        !started_while_held,
        "the chooser started inside another thread's spawn gate"
    );
    assert_eq!(pick.await.unwrap().unwrap(), Pick::Canceled);
    assert!(marker.exists());
}
