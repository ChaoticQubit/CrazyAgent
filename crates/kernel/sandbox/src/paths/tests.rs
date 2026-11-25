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

/// An owner home the account names through /System/Volumes/Data
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

/// A temp owner home holding `.ssh`, `Library/Keychains`, `.claude` and an unrelated `work`
/// folder. Output: the folder guard, the home and its secret paths with `.claude` as the
/// harness folder.
fn secret_home() -> (tempfile::TempDir, PathBuf, Vec<PathBuf>) {
    let folder = tempfile::tempdir().unwrap();
    let home = folder.path().join("home");
    for made in [".ssh", "Library/Keychains", ".claude", "work"] {
        std::fs::create_dir_all(home.join(made)).unwrap();
    }
    let secrets = owner_secrets(&home, &[".claude"]).collect();
    (folder, home, secrets)
}

/// True when the root is refused as one that holds or sits in a secret path. Inputs: the
/// root and the secret paths. Output: the answer.
fn secret_refused(root: &Path, secrets: &[PathBuf]) -> bool {
    matches!(
        clear_of_secrets(root, secrets),
        Err(SandboxError::Invalid(reason)) if reason == SECRET_ROOT_REFUSED
    )
}

/// On a volume that folds names, a root spelled in another case than a secret names the
/// secret: refused whether the secret exists or is still to come, and so is a root below it.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_root_spelled_in_another_case_than_a_secret_is_refused() {
    let (_folder, home, secrets) = secret_home();
    assert!(secret_refused(&home.join(".SSH"), &secrets));
    assert!(secret_refused(&home.join(".SSH").join("keys"), &secrets));
    assert!(secret_refused(&home.join(".Aws").join("new"), &secrets));
    assert!(secret_refused(
        &home.join(".s\u{200C}sh").join("new"),
        &secrets
    ));
}

/// A root spelled with KELVIN SIGN U+212A in place of `k`, which a folding volume reads as
/// `k`, names the keychains folder and is refused, as is a root below it.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_root_spelled_with_the_kelvin_sign_is_refused() {
    let (_folder, home, secrets) = secret_home();
    let kelvin = home.join("Library").join("\u{212A}eychains");
    assert!(secret_refused(&kelvin, &secrets));
    assert!(secret_refused(&kelvin.join("new"), &secrets));
    assert!(secret_refused(
        &home.join(".\u{212A}ube").join("new"),
        &secrets
    ));
}

/// A root that holds a secret through another spelling of the secret's folder is refused.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_root_holding_a_secret_spelled_in_another_case_is_refused() {
    let (_folder, home, secrets) = secret_home();
    assert!(secret_refused(&home.join("library"), &secrets));
    assert!(secret_refused(&home.join("LIBRARY"), &secrets));
}

/// A root under a folder its metadata cannot be read through is refused, since the check
/// cannot tell where it leads.
#[cfg(target_os = "macos")]
#[test]
fn a_metadata_read_error_refuses_the_root() {
    use std::os::unix::fs::PermissionsExt;
    let (_folder, home, secrets) = secret_home();
    let shut = home.join("work").join("shut");
    std::fs::create_dir_all(shut.join("inner")).unwrap();
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o000)).unwrap();
    let refused = secret_refused(&shut.join("inner"), &secrets);
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(refused);
}

/// A new folder whose name stays outside ASCII is allowed under an unrelated folder, and
/// refused where it could still name a secret: right under the home that holds them.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_new_folder_outside_ascii_is_refused_only_where_a_secret_may_be() {
    let (_folder, home, secrets) = secret_home();
    assert!(clear_of_secrets(&home.join("work").join("\u{65B0}"), &secrets).is_ok());
    assert!(secret_refused(&home.join("\u{65B0}"), &secrets));
}

/// The owner home inside a root spelled in another case, and a root inside a harness folder
/// spelled in another case, are each refused for that reason.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn granted_roots_compare_the_owner_and_harness_folders_by_entry() {
    let (folder, home, secrets) = secret_home();
    let refusal = |root: &Path| match granted_root(root, &home, &secrets, &[".claude"]) {
        Err(SandboxError::Invalid(reason)) => reason,
        other => format!("{other:?}"),
    };
    let holder = folder.path().join("HOME");
    assert_eq!(
        refusal(&holder),
        "sandbox roots must not contain the owner home"
    );
    let inside = home.join(".CLAUDE").join("projects");
    std::fs::create_dir_all(&inside).unwrap();
    assert_eq!(
        refusal(&inside),
        "sandbox roots must not be owner harness homes"
    );
}

/// On Linux names do not fold: `.SSH` beside `.ssh` is another folder and stays allowed,
/// while `.ssh` itself is refused.
#[cfg(target_os = "linux")]
#[test]
fn on_linux_a_root_spelled_in_another_case_is_another_folder() {
    let (_folder, home, secrets) = secret_home();
    std::fs::create_dir_all(home.join(".SSH")).unwrap();
    assert!(clear_of_secrets(&home.join(".SSH"), &secrets).is_ok());
    assert!(secret_refused(&home.join(".ssh"), &secrets));
}

/// A root that reaches a secret folder through a link is refused, and so is a root that
/// holds the folder a linked secret leads to.
#[cfg(unix)]
#[test]
fn a_root_reaching_a_secret_through_a_link_is_refused() {
    let (folder, home, secrets) = secret_home();
    std::fs::create_dir_all(home.join(".ssh").join("inner")).unwrap();
    let link = home.join("work").join("link");
    std::os::unix::fs::symlink(home.join(".ssh").join("inner"), &link).unwrap();
    assert!(secret_refused(&link.join("new"), &secrets));
    let dotfiles = folder.path().join("dotfiles");
    std::fs::create_dir_all(dotfiles.join("aws")).unwrap();
    std::os::unix::fs::symlink(dotfiles.join("aws"), home.join(".aws")).unwrap();
    assert!(secret_refused(&dotfiles, &secrets));
}

/// A harness home that is a link: the folder it leads to, a folder inside that and a folder
/// holding it are refused as an extra read-only folder is checked (its canonical form, then
/// the secret and granted-root checks), whether named through the link, in another case or
/// by their own path; and the harness check alone refuses the folders it leads to.
#[cfg(unix)]
#[test]
fn a_harness_home_that_is_a_link_refuses_the_folder_it_leads_to() {
    let (folder, home, secrets) = secret_home();
    let elsewhere = folder.path().join("elsewhere");
    let target = elsewhere.join("claude");
    std::fs::create_dir_all(target.join("projects")).unwrap();
    std::fs::remove_dir(home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&target, home.join(".claude")).unwrap();
    let mut typed = vec![
        home.join(".claude"),
        home.join(".claude").join("projects"),
        target.join("projects"),
        target.clone(),
        elsewhere.clone(),
    ];
    if cfg!(target_os = "macos") {
        typed.push(home.join(".CLAUDE"));
        typed.push(home.join(".Claude").join("projects"));
    }
    for typed in &typed {
        let real = canonical(typed).unwrap();
        assert!(secret_refused(&real, &secrets), "{}", typed.display());
        assert!(
            granted_root(&real, &home, &secrets, &[".claude"]).is_err(),
            "{}",
            typed.display()
        );
    }
    for inside in [target.clone(), target.join("projects")] {
        let real = canonical(&inside).unwrap();
        match granted_root(&real, &home, &[], &[".claude"]) {
            Err(SandboxError::Invalid(reason)) => {
                assert_eq!(reason, "sandbox roots must not be owner harness homes")
            }
            other => panic!("{}: {other:?}", inside.display()),
        }
    }
}

/// A secret home beside a `workspace` folder that holds `private` and `other`, with no
/// `.ssh` folder yet. Output: the temp folder, the home, its secrets and the canonical
/// workspace.
#[cfg(any(unix, windows))]
fn workspace_home() -> (tempfile::TempDir, PathBuf, Vec<PathBuf>, PathBuf) {
    let (folder, home, secrets) = secret_home();
    let workspace = folder.path().join("workspace");
    for made in ["private", "other"] {
        std::fs::create_dir_all(workspace.join(made)).unwrap();
    }
    std::fs::remove_dir(home.join(".ssh")).unwrap();
    let workspace = canonical(&workspace).unwrap();
    (folder, home, secrets, workspace)
}

/// The reason a granted root is refused for, or None when it is granted. Inputs: the root,
/// the owner home and the secret paths. Output: the reason.
#[cfg(any(unix, windows))]
fn granted_refusal(root: &Path, home: &Path, secrets: &[PathBuf]) -> Option<String> {
    match granted_root(root, home, secrets, &[".claude"]) {
        Ok(()) => None,
        Err(SandboxError::Invalid(reason)) => Some(reason),
        Err(other) => Some(format!("{other:?}")),
    }
}

/// A secret link whose target does not exist yet refuses a root that holds where it leads,
/// whether the target is absolute, relative with a `..` part, or another such link named
/// relative to the home; a folder beside that root stays allowed.
#[cfg(unix)]
#[test]
fn a_root_holding_where_a_dangling_secret_link_leads_is_refused() {
    use std::os::unix::fs::symlink;
    for case in ["absolute", "relative", "chained"] {
        let (folder, home, secrets, workspace) = workspace_home();
        let private = workspace.join("private");
        let made = match case {
            "absolute" => symlink(
                folder.path().join("workspace").join("private").join("ssh"),
                home.join(".ssh"),
            ),
            "relative" => {
                std::fs::create_dir(workspace.join("x")).unwrap();
                symlink("../workspace/x/../private/gnupg", home.join(".gnupg"))
            }
            "chained" => {
                std::fs::create_dir(folder.path().join("links")).unwrap();
                symlink(private.join("aws"), folder.path().join("links").join("aws")).unwrap();
                symlink("../links/aws", home.join(".aws"))
            }
            _ => unreachable!(),
        };
        made.unwrap();
        assert!(secret_refused(&private, &secrets), "{case}");
        assert_eq!(
            granted_refusal(&private, &home, &secrets).as_deref(),
            Some(SECRET_ROOT_REFUSED),
            "{case}"
        );
        let other = workspace.join("other");
        assert!(clear_of_secrets(&other, &secrets).is_ok(), "{case}");
        assert_eq!(granted_refusal(&other, &home, &secrets), None, "{case}");
    }
}

/// A dangling secret link refuses only the folders at and above the deepest existing folder
/// it leads into: a sibling of the parts still to be made below that folder stays allowed.
#[cfg(unix)]
#[test]
fn a_dangling_secret_link_refuses_only_the_folders_above_where_it_leads() {
    let (folder, home, secrets, workspace) = workspace_home();
    let target = folder
        .path()
        .join("workspace")
        .join("a")
        .join("b")
        .join("ssh");
    std::os::unix::fs::symlink(target, home.join(".ssh")).unwrap();
    let private = workspace.join("private");
    assert!(clear_of_secrets(&private, &secrets).is_ok());
    assert_eq!(granted_refusal(&private, &home, &secrets), None);
    assert!(secret_refused(&workspace, &secrets));
    assert_eq!(
        granted_refusal(&workspace, &home, &secrets).as_deref(),
        Some(SECRET_ROOT_REFUSED)
    );
}

/// A cycle of dangling secret links, and a chain of twenty of them, which the system still
/// follows, refuse every root rather than loop or stop partway.
#[cfg(unix)]
#[test]
fn dangling_secret_links_in_a_cycle_or_a_long_chain_are_refused() {
    use std::os::unix::fs::symlink;
    let (folder, home, secrets, workspace) = workspace_home();
    let ring = folder.path().join("ring");
    std::fs::create_dir(&ring).unwrap();
    symlink(ring.join("b"), ring.join("a")).unwrap();
    symlink(ring.join("a"), ring.join("b")).unwrap();
    symlink(ring.join("a"), home.join(".ssh")).unwrap();
    let other = workspace.join("other");
    assert_eq!(
        granted_refusal(&other, &home, &secrets).as_deref(),
        Some(SECRET_ROOT_REFUSED)
    );
    let (folder, home, secrets, workspace) = workspace_home();
    let chain = canonical(folder.path()).unwrap().join("chain");
    std::fs::create_dir(&chain).unwrap();
    for step in 0..20 {
        symlink(
            chain.join(format!("l{}", step + 1)),
            chain.join(format!("l{step}")),
        )
        .unwrap();
    }
    symlink(chain.join("l0"), home.join(".ssh")).unwrap();
    const { assert!(DANGLING_LINK_HOPS < 20) };
    assert!(
        std::fs::metadata(home.join(".ssh"))
            .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
    );
    let other = workspace.join("other");
    assert_eq!(
        granted_refusal(&other, &home, &secrets).as_deref(),
        Some(SECRET_ROOT_REFUSED)
    );
}

/// On Windows a secret folder that is a dangling directory link, or a dangling junction made
/// by `mklink /J` whose target was then removed, refuses a root that holds where it leads,
/// while a folder beside that root stays allowed. A link the row cannot make fails the row.
#[cfg(windows)]
#[test]
fn on_windows_a_root_holding_where_a_dangling_secret_link_or_junction_leads_is_refused() {
    for case in ["symlink", "junction"] {
        let (_folder, home, secrets, workspace) = workspace_home();
        let private = workspace.join("private");
        let target = private.join("ssh");
        let link = home.join(".ssh");
        match case {
            "symlink" => std::os::windows::fs::symlink_dir(&target, &link).unwrap(),
            "junction" => {
                std::fs::create_dir(&target).unwrap();
                let made = kernel_processes::spawn_gate::status(
                    std::process::Command::new("cmd")
                        .args(["/C", "mklink", "/J"])
                        .arg(&link)
                        .arg(&target),
                )
                .unwrap();
                assert!(made.success(), "mklink /J {}", link.display());
                std::fs::remove_dir(&target).unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "{case}"
        );
        assert!(
            std::fs::metadata(&link)
                .is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound),
            "{case}"
        );
        assert!(secret_refused(&private, &secrets), "{case}");
        assert_eq!(
            granted_refusal(&private, &home, &secrets).as_deref(),
            Some(SECRET_ROOT_REFUSED),
            "{case}"
        );
        let other = workspace.join("other");
        assert!(clear_of_secrets(&other, &secrets).is_ok(), "{case}");
        assert_eq!(granted_refusal(&other, &home, &secrets), None, "{case}");
    }
}
