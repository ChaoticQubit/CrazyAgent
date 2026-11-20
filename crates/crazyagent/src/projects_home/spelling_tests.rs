use super::windows_plain;

#[test]
fn a_verbatim_disk_path_turns_plain() {
    assert_eq!(
        windows_plain(r"\\?\C:\Users\me\CrazyAgent"),
        Some(r"C:\Users\me\CrazyAgent")
    );
    assert_eq!(windows_plain(r"\\?\d:\data"), Some(r"d:\data"));
}

#[test]
fn an_already_plain_path_is_unchanged() {
    assert_eq!(
        windows_plain(r"C:\Users\me\CrazyAgent"),
        Some(r"C:\Users\me\CrazyAgent")
    );
}

#[test]
fn a_unc_path_is_refused_as_the_sandbox_refuses_every_unc_path() {
    assert_eq!(windows_plain(r"\\?\UNC\server\share\folder"), None);
    assert_eq!(windows_plain(r"\\server\share\folder"), None);
}

#[test]
fn device_and_other_verbatim_forms_are_refused() {
    for text in [
        r"\\.\C:\Users\me",
        r"\\.\pipe\name",
        r"\\?\Volume{0b1c2d3e-0000-0000-0000-000000000000}\data",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1\data",
        r"\??\C:\Users\me",
    ] {
        assert_eq!(windows_plain(text), None, "{text}");
    }
}

#[test]
fn a_name_win32_would_normalise_is_refused_in_either_spelling() {
    for text in [
        r"\\?\C:\Users\me.",
        r"\\?\C:\Users\me ",
        r"\\?\C:\Users\..\me",
        r"\\?\C:\Users/me",
        r"C:\Users\me.",
    ] {
        assert_eq!(windows_plain(text), None, "{text}");
    }
}

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
