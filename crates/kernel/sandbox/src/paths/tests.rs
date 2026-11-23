use super::*;

#[test]
fn a_plain_drive_path_is_clean_on_every_system() {
    for text in [
        r"C:\Users\owner\project",
        r"d:\work\agent home\tmp",
        r"C:\a",
    ] {
        assert!(windows_clean_text(text), "{text}");
    }
}

#[test]
fn unc_verbatim_device_and_relative_paths_are_refused() {
    for text in [
        r"\\server\share\project",
        r"\\?\C:\project",
        r"\\?\UNC\server\share",
        r"\\.\PhysicalDrive0",
        r"C:project",
        r"project\folder",
        r"\project",
        "C:/project",
        r"C:\",
    ] {
        assert!(!windows_clean_text(text), "{text}");
    }
}

#[test]
fn dots_quotes_streams_and_aliases_are_refused() {
    for text in [
        r"C:\project\..\owner",
        r"C:\project\.\context",
        r"C:\project\context.",
        r"C:\project\context ",
        r"C:\project\\context",
        r"C:\project\",
        r"C:\project\file.txt:stream",
        "C:\\project\\it's",
        "C:\\project\\\"quoted\"",
        "C:\\project\\line\nbreak",
        r"C:\project\a/b",
        r"C:\project\*",
        r"C:\project\a?b",
        r"C:\project\a|b",
    ] {
        assert!(!windows_clean_text(text), "{text:?}");
    }
}

#[test]
fn a_path_longer_than_the_limit_is_refused() {
    let long = format!(r"C:\{}", "a".repeat(PATH_BYTES_MAX));
    assert!(!windows_clean_text(&long));
    let fits = format!(r"C:\{}", "a".repeat(PATH_BYTES_MAX - 3));
    assert!(windows_clean_text(&fits));
}

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

/// V243-FIRMLINK-SWEEP D1: an owner home the account names through /System/Volumes/Data
/// settles to its short name, the spelling every sandbox root has, so the owner home and
/// secret denies built from it still match.
#[cfg(target_os = "macos")]
#[test]
fn a_home_named_through_the_data_volume_settles_to_its_short_name() {
    let base = tempfile::tempdir().unwrap();
    let short = base.path().canonicalize().unwrap();
    let long = PathBuf::from(format!("/System/Volumes/Data{}", short.display()));
    assert!(long.is_dir());
    assert_eq!(settled_home(long).unwrap(), short);
    assert_eq!(settled_home(short.clone()).unwrap(), short);
}

/// The harness folders a caller hands in reach the owner's secret list between the fixed
/// names before and after them, each under the owner home, followed by the system's
/// secret paths, in that order.
#[test]
fn the_harness_folders_reach_the_secret_list() {
    let owner = Path::new("/home/owner");
    let found: Vec<PathBuf> = owner_secrets(owner, &[".fixture-a", ".fixture-b/inner"]).collect();
    let under = |parts: &[&str]| {
        parts
            .iter()
            .fold(owner.to_path_buf(), |at, part| at.join(part))
    };
    let mut expected: Vec<PathBuf> = [
        &["Library", "Keychains"][..],
        &[".ssh"],
        &[".gnupg"],
        &[".netrc"],
        &[".git-credentials"],
        &[".config", "gh"],
        &[".config", "git", "credentials"],
        &[".aws"],
        &[".docker"],
        &[".kube"],
        &[".fixture-a"],
        &[".fixture-b", "inner"],
        &[".cargo", "credentials"],
        &[".cargo", "credentials.toml"],
        &[".npmrc"],
        &[".pypirc"],
        &[".config", "gcloud"],
    ]
    .into_iter()
    .map(under)
    .collect();
    expected.extend(
        [
            "/Library/Keychains",
            "/private/var/db/SystemKey",
            "/opt/homebrew/etc/npmrc",
            "/usr/local/etc/npmrc",
        ]
        .map(PathBuf::from),
    );
    assert_eq!(found, expected);
}

/// The table of code points a case-insensitive volume folds onto ASCII is defined in one
/// place in the workspace, the sandbox path module, so the landing checks, the name compares
/// and the Seatbelt spellings read the same folds.
#[test]
fn ascii_folds_has_one_home() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let needle = ["const ASCII", "_FOLDS"].concat();
    let mut homes = Vec::new();
    let mut folders = vec![root.join("crates"), root.join("apps/desktop/src-tauri")];
    while let Some(folder) = folders.pop() {
        for entry in std::fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            if path.is_dir() && name != "target" && !name.starts_with('.') {
                folders.push(path);
            } else if name.ends_with(".rs")
                && std::fs::read_to_string(&path)
                    .unwrap()
                    .lines()
                    .any(|line| line.contains(&needle))
            {
                homes.push(path.strip_prefix(&root).unwrap().to_path_buf());
            }
        }
    }
    assert_eq!(homes, [Path::new("crates/kernel/sandbox/src/paths.rs")]);
}
