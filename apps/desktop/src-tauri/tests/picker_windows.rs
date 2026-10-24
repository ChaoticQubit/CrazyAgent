#![cfg(target_os = "windows")]

use crazyagent_desktop::bridge::picker::{
    DOCUMENTS_SCRIPT, FOLDER_SCRIPT, Pick, WINDOWS_DOCUMENTS_SCRIPT, WINDOWS_FOLDER_SCRIPT,
    chooser_command, read_reply,
};

#[test]
fn windows_choosers_run_fixed_powershell_scripts_without_double_quotes() {
    for (script, windows) in [
        (FOLDER_SCRIPT, WINDOWS_FOLDER_SCRIPT),
        (DOCUMENTS_SCRIPT, WINDOWS_DOCUMENTS_SCRIPT),
    ] {
        let command = chooser_command(script).unwrap();
        assert!(
            command
                .get_program()
                .to_string_lossy()
                .ends_with(r"WindowsPowerShell\v1.0\powershell.exe")
        );
        assert_eq!(
            command.get_args().last(),
            Some(std::ffi::OsStr::new(windows))
        );
        assert!(!windows.contains('"'));
        assert!(windows.contains("System.Windows.Forms"));
    }
    assert!(WINDOWS_FOLDER_SCRIPT.contains("FolderBrowserDialog"));
    assert!(WINDOWS_DOCUMENTS_SCRIPT.contains("$d.Multiselect = $true"));
}

#[test]
fn windows_replies_keep_drive_paths_and_roots() {
    let pick = read_reply(true, b"C:\\Users\\owner\\skills\\design\\\r\n", b"").unwrap();
    assert_eq!(
        pick,
        Pick::Paths(vec![r"C:\Users\owner\skills\design".into()])
    );
    let root = read_reply(true, b"D:\\\r\n", b"").unwrap();
    assert_eq!(root, Pick::Paths(vec![r"D:\".into()]));
    let several = read_reply(true, b"C:\\a.md\nC:\\b c.pdf\n", b"").unwrap();
    assert_eq!(
        several,
        Pick::Paths(vec![r"C:\a.md".into(), r"C:\b c.pdf".into()])
    );
    assert_eq!(read_reply(true, b"", b"").unwrap(), Pick::Canceled);
    assert_eq!(
        read_reply(true, b"relative\\path\n", b"").unwrap_err().code,
        "picker_failed"
    );
}
