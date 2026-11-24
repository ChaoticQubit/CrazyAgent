#[test]
fn a_containment_compare_reads_every_verbatim_drive_path_plain_and_nothing_else() {
    use super::windows_drive_plain;
    assert_eq!(
        windows_drive_plain(r"\\?\C:\work\it's"),
        Some(r"C:\work\it's")
    );
    assert_eq!(windows_drive_plain(r"\\?\C:\a.\b"), Some(r"C:\a.\b"));
    assert_eq!(windows_drive_plain(r"\\?\C:\"), Some(r"C:\"));
    for other in [
        r"C:\work",
        r"\\?\C:",
        r"\\?\UNC\server\share",
        r"\\?\Volume{0}\work",
        r"\\.\C:\work",
        r"\??\C:\work",
        r"\\server\share",
    ] {
        assert_eq!(windows_drive_plain(other), None, "{other}");
    }
}
