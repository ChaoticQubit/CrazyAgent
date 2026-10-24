#![cfg(unix)]

use crazyagent_desktop::bridge::picker::{
    DOCUMENTS_SCRIPT, FOLDER_SCRIPT, Pick, chooser_command, read_reply,
};

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

#[cfg(target_os = "macos")]
#[test]
fn macos_choosers_run_the_fixed_scripts_through_osascript() {
    for script in [FOLDER_SCRIPT, DOCUMENTS_SCRIPT] {
        let command = chooser_command(script).unwrap();
        assert_eq!(command.get_program(), "/usr/bin/osascript");
        assert!(command.get_args().eq(["-e", script]));
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
    let crlf = read_reply(true, b"/home/owner/skills/\r\n", b"").unwrap();
    assert_eq!(crlf, Pick::Paths(vec!["/home/owner/skills".into()]));
}
