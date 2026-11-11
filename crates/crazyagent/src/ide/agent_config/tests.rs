use super::*;

/// The `.vscode` names each candidate is, or is not, on a case-insensitive APFS volume:
/// letter case and the two non-ASCII letters APFS folds to ASCII match; accents in composed
/// or decomposed form, invisible and full-width characters do not.
const FOLDER_NAMES: [(&str, bool); 13] = [
    (".vscode", true),
    (".VSCode", true),
    (".VSCODE", true),
    (".v\u{17f}code", true),
    (".V\u{17f}CODE", true),
    (".vscod\u{e9}", false),
    (".vscode\u{301}", false),
    (".vscod\u{c9}", false),
    (".vsc\u{200b}ode", false),
    ("\u{ff0e}vscode", false),
    (".\u{ff56}scode", false),
    (".vscodex", false),
    ("vscode", false),
];

/// The workspace file names each candidate is, or is not, on the same volume.
const WORKSPACE_NAMES: [(&str, bool); 6] = [
    ("x.code-workspace", true),
    ("X.CODE-WORKSPACE", true),
    ("x.code-wor\u{212a}space", true),
    ("x.code-work\u{17f}pace", true),
    ("x.code-workspac\u{e9}", false),
    ("x.code-workspace\u{301}", false),
];

/// Every non-ASCII code point a real APFS volume opens as a printable ASCII character,
/// with that character.
const FOLDS: [(char, char); 4] = [
    ('\u{17f}', 's'),
    ('\u{212a}', 'k'),
    ('\u{37e}', ';'),
    ('\u{1fef}', '`'),
];

#[test]
fn ide_config_paths_fold_case_the_way_apfs_does() {
    for (name, same) in FOLDER_NAMES {
        assert_eq!(same_name(name, FOLDER), same, "{name:?}");
        assert_eq!(
            is_ide_config(&format!("{name}/settings.json")),
            same,
            "{name:?}"
        );
        assert_eq!(
            is_ide_config(&format!("web/{name}/tasks.json")),
            same,
            "{name:?}"
        );
    }
    for (name, same) in WORKSPACE_NAMES {
        assert_eq!(is_ide_config(name), same, "{name:?}");
        assert_eq!(is_ide_config(&format!("docs/{name}")), same, "{name:?}");
    }
    for (folded, ascii) in FOLDS {
        assert!(same_name(&format!("a{folded}b"), &format!("a{ascii}b")));
        assert!(!same_name(&format!("a{folded}b"), "ab"));
    }
    assert!(same_name(".GITATTRIBUTE\u{17f}", ".gitattributes"));
    assert!(same_name(".GitModules", ".gitmodules"));
    assert!(!same_name(".gitmodule", ".gitmodules"));
    for plain in [
        "vscode/notes.md",
        "src/.vscode-old/a.txt",
        "notes.code-workspace.md",
        ".code-workspaces/a.txt",
        "",
    ] {
        assert!(!is_ide_config(plain), "{plain:?}");
    }
}

/// The matcher agrees with a real case-insensitive APFS volume: each candidate name opens
/// the real `.vscode` folder or workspace file exactly when the matcher says it is that
/// name, and the volume itself treats composed and decomposed accents as one name.
#[cfg(target_os = "macos")]
#[test]
fn ide_config_names_match_what_the_real_volume_opens() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join("probe"), "").unwrap();
    assert!(
        root.join("PROBE").exists(),
        "the temporary folder must be on a case-insensitive volume, as macOS installs it"
    );
    std::fs::write(root.join("caf\u{e9}"), "").unwrap();
    assert!(root.join("cafe\u{301}").exists());
    assert!(root.join("CAF\u{c9}").exists());
    for (folded, ascii) in FOLDS {
        let plain = root.join(format!("q{ascii}"));
        std::fs::write(&plain, "").unwrap();
        let opened = root.join(format!("q{folded}"));
        assert!(
            opened.exists() && same_file(&opened, &plain),
            "{folded:?} on the volume"
        );
    }
    std::fs::create_dir(root.join(FOLDER)).unwrap();
    for (name, same) in FOLDER_NAMES {
        assert_eq!(root.join(name).is_dir(), same, "{name:?} on the volume");
        assert_eq!(same_name(name, FOLDER), same, "{name:?}");
    }
    let workspace = root.join("x.code-workspace");
    std::fs::write(&workspace, "{}").unwrap();
    for (name, same) in WORKSPACE_NAMES {
        let opened = root.join(name);
        assert_eq!(
            opened.is_file() && same_file(&opened, &workspace),
            same,
            "{name:?} on the volume"
        );
        assert_eq!(is_ide_config(name), same, "{name:?}");
    }
}

/// Whether two paths name one file. Inputs: the paths. Output: true for the same inode.
#[cfg(target_os = "macos")]
fn same_file(left: &std::path::Path, right: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (left, right) = (
        std::fs::metadata(left).unwrap(),
        std::fs::metadata(right).unwrap(),
    );
    (left.dev(), left.ino()) == (right.dev(), right.ino())
}
