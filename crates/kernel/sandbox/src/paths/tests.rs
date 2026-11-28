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

/// Checks a path as a sandbox build does, through a [`Kept`] of the owner home, `.claude`
/// and `secrets`, and [`Kept::clear`]. Inputs: the owner home, the path and the secret paths.
/// Output: unit, or the build's refusal.
fn kept_clear(home: &Path, root: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
    Kept::of(home, &harness_homes(home, &[".claude"]), secrets)?.clear(root, secrets)
}

/// Checks a root as a sandbox build grants it, through a [`Kept`] of the owner home,
/// `.claude` and `secrets`, and [`Kept::grant`]. Inputs: the root, the owner home and the
/// secret paths, which hold `.claude`. Output: unit, or the build's refusal.
fn kept_grant(root: &Path, home: &Path, secrets: &[PathBuf]) -> Result<(), SandboxError> {
    Kept::of(home, &harness_homes(home, &[".claude"]), secrets)?.grant(root, secrets)
}

/// True when the root is refused as one that holds or sits in a secret path. Inputs: the
/// owner home, the root and the secret paths. Output: the answer.
fn secret_refused(home: &Path, root: &Path, secrets: &[PathBuf]) -> bool {
    matches!(
        kept_clear(home, root, secrets),
        Err(SandboxError::Invalid(reason)) if reason == SECRET_ROOT_REFUSED
    )
}

/// On a volume that folds names, a root spelled in another case than a secret names the
/// secret: refused whether the secret exists or is still to come, and so is a root below it.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_root_spelled_in_another_case_than_a_secret_is_refused() {
    let (_folder, home, secrets) = secret_home();
    assert!(secret_refused(&home, &home.join(".SSH"), &secrets));
    assert!(secret_refused(
        &home,
        &home.join(".SSH").join("keys"),
        &secrets
    ));
    assert!(secret_refused(
        &home,
        &home.join(".Aws").join("new"),
        &secrets
    ));
    assert!(secret_refused(
        &home,
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
    assert!(secret_refused(&home, &kelvin, &secrets));
    assert!(secret_refused(&home, &kelvin.join("new"), &secrets));
    assert!(secret_refused(
        &home,
        &home.join(".\u{212A}ube").join("new"),
        &secrets
    ));
}

/// A root that holds a secret through another spelling of the secret's folder is refused.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_root_holding_a_secret_spelled_in_another_case_is_refused() {
    let (_folder, home, secrets) = secret_home();
    assert!(secret_refused(&home, &home.join("library"), &secrets));
    assert!(secret_refused(&home, &home.join("LIBRARY"), &secrets));
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
    let refused = secret_refused(&home, &shut.join("inner"), &secrets);
    std::fs::set_permissions(&shut, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(refused);
}

/// A new folder whose name stays outside ASCII is allowed under an unrelated folder, and
/// refused where it could still name a secret: right under the home that holds them.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn a_new_folder_outside_ascii_is_refused_only_where_a_secret_may_be() {
    let (_folder, home, secrets) = secret_home();
    assert!(kept_clear(&home, &home.join("work").join("\u{65B0}"), &secrets).is_ok());
    assert!(secret_refused(&home, &home.join("\u{65B0}"), &secrets));
}

/// The owner home inside a root spelled in another case, and a root inside a harness folder
/// spelled in another case, are each refused for that reason.
#[cfg(any(target_os = "macos", windows))]
#[test]
fn granted_roots_compare_the_owner_and_harness_folders_by_entry() {
    let (folder, home, secrets) = secret_home();
    let refusal = |root: &Path| match kept_grant(root, &home, &secrets) {
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
    assert!(kept_clear(&home, &home.join(".SSH"), &secrets).is_ok());
    assert!(secret_refused(&home, &home.join(".ssh"), &secrets));
}

/// A root not made yet, named through a link to the folder that holds a secret still to
/// come, is refused as the secret itself, and so is a root below it; a secret named through
/// such a link refuses the root it names.
#[cfg(unix)]
#[test]
fn a_new_root_through_a_link_to_a_missing_secret_is_refused() {
    let (folder, home, secrets) = secret_home();
    assert!(!home.join(".aws").exists());
    let alias = folder.path().join("alias");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    assert!(secret_refused(&home, &alias.join(".aws"), &secrets));
    assert!(secret_refused(
        &home,
        &alias.join(".aws").join("new"),
        &secrets
    ));
    assert!(secret_refused(
        &home,
        &home.join(".aws"),
        &[alias.join(".aws")]
    ));
}

/// On Linux names do not fold: a root not made yet, named through a link to the folder that
/// holds a secret still to come, is another folder when its name differs from the secret's,
/// also only in case or outside ASCII.
#[cfg(target_os = "linux")]
#[test]
fn on_linux_a_new_root_beside_a_missing_secret_is_not_refused() {
    let (folder, home, secrets) = secret_home();
    let alias = folder.path().join("alias");
    std::os::unix::fs::symlink(&home, &alias).unwrap();
    for name in ["project", ".AWS", "\u{65B0}"] {
        assert!(
            kept_clear(&home, &alias.join(name), &secrets).is_ok(),
            "{name}"
        );
    }
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
    assert!(secret_refused(&home, &link.join("new"), &secrets));
    let dotfiles = folder.path().join("dotfiles");
    std::fs::create_dir_all(dotfiles.join("aws")).unwrap();
    std::os::unix::fs::symlink(dotfiles.join("aws"), home.join(".aws")).unwrap();
    assert!(secret_refused(&home, &dotfiles, &secrets));
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
        assert!(
            secret_refused(&home, &real, &secrets),
            "{}",
            typed.display()
        );
        assert!(
            kept_grant(&real, &home, &secrets).is_err(),
            "{}",
            typed.display()
        );
    }
    for inside in [target.clone(), target.join("projects")] {
        let real = canonical(&inside).unwrap();
        match kept_grant(&real, &home, &[home.join(".claude")]) {
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
    match kept_grant(root, home, secrets) {
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
        assert!(secret_refused(&home, &private, &secrets), "{case}");
        assert_eq!(
            granted_refusal(&private, &home, &secrets).as_deref(),
            Some(SECRET_ROOT_REFUSED),
            "{case}"
        );
        let other = workspace.join("other");
        assert!(kept_clear(&home, &other, &secrets).is_ok(), "{case}");
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
    assert!(kept_clear(&home, &private, &secrets).is_ok());
    assert_eq!(granted_refusal(&private, &home, &secrets), None);
    assert!(secret_refused(&home, &workspace, &secrets));
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
        assert!(secret_refused(&home, &private, &secrets), "{case}");
        assert_eq!(
            granted_refusal(&private, &home, &secrets).as_deref(),
            Some(SECRET_ROOT_REFUSED),
            "{case}"
        );
        let other = workspace.join("other");
        assert!(kept_clear(&home, &other, &secrets).is_ok(), "{case}");
        assert_eq!(granted_refusal(&other, &home, &secrets), None, "{case}");
    }
}

/// A build that grants `root` against the owner home, `.claude` and every owner secret,
/// as a sandbox build keeps it. Inputs: the owner home, its secrets and the root. Output:
/// the kept build.
#[cfg(unix)]
fn granted_build(home: &Path, secrets: &[PathBuf], root: &Path) -> Kept {
    let mut kept = Kept::of(home, &harness_homes(home, &[".claude"]), secrets).unwrap();
    kept.grant(root, secrets).unwrap();
    kept
}

/// A harness home replaced by a link to a root after the build granted that root is
/// refused at install: the harness home is one of the secrets every grant is checked
/// against, so the install's fresh walk of it reaches the root.
#[cfg(unix)]
#[test]
fn a_harness_home_made_a_link_to_a_granted_root_after_the_build_is_refused_at_install() {
    let (folder, home, secrets) = secret_home();
    let root = canonical(folder.path()).unwrap().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let kept = granted_build(&home, &secrets, &root);
    assert!(kept.recheck().is_ok());
    std::fs::remove_dir(home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&root, home.join(".claude")).unwrap();
    match kept.recheck() {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, SECRET_ROOT_REFUSED),
        other => panic!("{other:?}"),
    }
}

/// An owner home moved into a granted root after the build, its old path now a link to
/// it, is refused at install: every owner secret sits under the owner home, so the
/// install's fresh walks of them reach the root.
#[cfg(unix)]
#[test]
fn an_owner_home_moved_into_a_granted_root_after_the_build_is_refused_at_install() {
    let (folder, home, secrets) = secret_home();
    let root = canonical(folder.path()).unwrap().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let kept = granted_build(&home, &secrets, &root);
    assert!(kept.recheck().is_ok());
    let moved = root.join("home");
    std::fs::rename(&home, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &home).unwrap();
    match kept.recheck() {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, SECRET_ROOT_REFUSED),
        other => panic!("{other:?}"),
    }
}

/// A grant checked against a list that leaves out a harness home trips the build's
/// assertion, since the install would then not judge that home again.
#[cfg(unix)]
#[test]
#[should_panic(expected = "a grant is checked against every harness home")]
fn a_grant_against_a_list_without_a_harness_home_trips_the_build() {
    let (folder, home, secrets) = secret_home();
    let root = canonical(folder.path()).unwrap().join("project");
    std::fs::create_dir_all(&root).unwrap();
    let without: Vec<PathBuf> = secrets
        .into_iter()
        .filter(|secret| *secret != home.join(".claude"))
        .collect();
    let _ = granted_build(&home, &without, &root);
}

/// A canonical scratch folder, so no run above it is a link. Output: the folder guard and
/// the folder.
#[cfg(unix)]
fn plain_scratch() -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let real = folder.path().canonicalize().unwrap();
    (folder, real)
}

/// Asserts that [`Walk::read`] and, when it answers, [`Walk::read_fast`] give the same whole
/// walk as [`Walk::read_full`]: path, run identities and reached identities in order. Input:
/// the path. Output: true when the fast read answered.
#[cfg(unix)]
fn walks_as_full(path: &Path) -> bool {
    let full = Walk::read_full(path, &mut Reads::default()).unwrap();
    let fast = Walk::read_fast(path, &mut Reads::default()).unwrap();
    let read = Walk::read(path, &mut Reads::default()).unwrap();
    for walk in std::iter::once(&read).chain(fast.as_ref()) {
        assert_eq!(walk.path, full.path, "{}", path.display());
        assert_eq!(walk.ids, full.ids, "{}", path.display());
        assert_eq!(walk.reached, full.reached, "{}", path.display());
    }
    fast.is_some()
}

/// A path with a link in a run walks as the full read, through the fallback, and so does
/// a `..` part; the same path through the link's target takes the fast read. A list that
/// walked the link first still reads the link's run as its target, not as the link.
#[cfg(unix)]
#[test]
fn a_link_in_a_run_walks_as_the_full_read() {
    let (_folder, root) = plain_scratch();
    std::fs::create_dir_all(root.join("real").join("inner")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    assert!(!walks_as_full(&root.join("link").join("inner").join("new")));
    assert!(!walks_as_full(&root.join("real").join("..").join("real")));
    assert!(walks_as_full(&root.join("real").join("inner").join("new")));
    let mut shared = Reads::default();
    let through = root.join("link").join("inner");
    Walk::read(&root.join("link"), &mut shared).unwrap();
    let again = Walk::read(&through, &mut shared).unwrap();
    let full = Walk::read_full(&through, &mut Reads::default()).unwrap();
    assert_eq!(again.ids, full.ids);
    assert_eq!(again.reached, full.reached);
}

/// A dangling link in the middle of a path walks as the full read, through the fallback.
#[cfg(unix)]
#[test]
fn a_mid_path_dangling_link_walks_as_the_full_read() {
    let (_folder, root) = plain_scratch();
    std::os::unix::fs::symlink(root.join("nowhere"), root.join("gone")).unwrap();
    assert!(!walks_as_full(
        &root.join("gone").join("child").join("leaf")
    ));
}

/// A dangling link as the last part of a path walks as the full read, through the fallback.
#[cfg(unix)]
#[test]
fn a_last_part_dangling_link_walks_as_the_full_read() {
    let (_folder, root) = plain_scratch();
    std::os::unix::fs::symlink(root.join("nowhere"), root.join("gone")).unwrap();
    assert!(!walks_as_full(&root.join("gone")));
}

/// A link to a secret folder walks as the full read and still reaches the secret's identity.
#[cfg(unix)]
#[test]
fn a_link_to_a_secret_walks_as_the_full_read_and_reaches_the_secret() {
    let (_folder, root) = plain_scratch();
    let secret = root.join("secret");
    std::fs::create_dir(&secret).unwrap();
    std::os::unix::fs::symlink(&secret, root.join("alias")).unwrap();
    assert!(!walks_as_full(&root.join("alias")));
    let walk = Walk::read(&root.join("alias"), &mut Reads::default()).unwrap();
    let secret_id = Walk::read(&secret, &mut Reads::default())
        .unwrap()
        .ids
        .last()
        .copied()
        .flatten()
        .unwrap();
    assert!(walk.reached.contains(&secret_id));
}

/// A root spelled in another case than on disk under a home takes the fast read and walks
/// as the full read: the walk keeps the spelled text and the same identities.
#[cfg(target_os = "macos")]
#[test]
fn a_wrong_case_root_under_a_home_takes_the_fast_read_as_the_full_read() {
    let (_folder, root) = plain_scratch();
    std::fs::create_dir_all(root.join("home").join("Work")).unwrap();
    let spelled = root.join("home").join("WORK").join("new");
    let full = Walk::read_full(&spelled, &mut Reads::default()).unwrap();
    assert!(full.ids[full.ids.len() - 2].is_some(), "volume folds names");
    assert!(walks_as_full(&spelled));
}

/// The owner home under the `/Users` firmlink and a new path below it take the fast read
/// and walk as the full read.
#[cfg(target_os = "macos")]
#[test]
fn a_path_under_users_takes_the_fast_read_as_the_full_read() {
    let home = owner_home().unwrap();
    assert!(home.starts_with("/Users"));
    assert!(walks_as_full(&home));
    assert!(walks_as_full(&home.join("ca-fast-read-row-no-such-folder")));
}

/// A canonical scratch folder holding a folder `x` with two folders `a` and `b` in it, and
/// a secret home beside them. Output: the guard, the home, its secrets and the folder `x`.
#[cfg(unix)]
fn two_roots_under_one_folder() -> (tempfile::TempDir, PathBuf, Vec<PathBuf>, PathBuf) {
    let (folder, home, secrets) = secret_home();
    let x = canonical(folder.path()).unwrap().join("x");
    for leaf in ["a", "b"] {
        std::fs::create_dir_all(x.join(leaf)).unwrap();
    }
    (folder, home, secrets, x)
}

/// A build's second grant under a folder its first grant walked reads only the new root's
/// own entry, since one build shares its reads.
#[cfg(unix)]
#[test]
fn a_second_grant_under_a_walked_folder_reads_only_its_own_entry() {
    let (_folder, home, secrets, x) = two_roots_under_one_folder();
    let mut kept = granted_build(&home, &secrets, &x.join("a"));
    let before = entry_reads();
    kept.grant(&x.join("b"), &secrets).unwrap();
    assert_eq!(entry_reads() - before, 1);
}

/// A folder swapped for a new one between two grants of one build is judged from the
/// build's first read until the install, which reads it again and refuses with
/// [`GRANT_CHANGED`].
#[cfg(unix)]
#[test]
fn a_folder_swapped_between_two_grants_of_one_build_refuses_the_install() {
    let (folder, home, secrets, x) = two_roots_under_one_folder();
    let mut kept = granted_build(&home, &secrets, &x.join("a"));
    std::fs::rename(&x, folder.path().join("x-old")).unwrap();
    for leaf in ["a", "b"] {
        std::fs::create_dir_all(x.join(leaf)).unwrap();
    }
    kept.grant(&x.join("b"), &secrets).unwrap();
    match kept.recheck() {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, GRANT_CHANGED),
        other => panic!("{other:?}"),
    }
}

/// A folder swapped for a link into a secret between two grants of one build: the second
/// grant's walk reads the folder from the build's reads and so misses the secret, but its
/// fresh canonical form differs from the path, so the grant refuses it as not canonical,
/// and the install, reading the first grant's folder again, refuses with [`GRANT_CHANGED`].
#[cfg(unix)]
#[test]
fn a_folder_swapped_for_a_link_into_a_secret_between_two_grants_is_refused() {
    let (folder, home, secrets, x) = two_roots_under_one_folder();
    let mut kept = granted_build(&home, &secrets, &x.join("a"));
    std::fs::rename(&x, folder.path().join("x-old")).unwrap();
    std::fs::create_dir_all(home.join(".ssh").join("b")).unwrap();
    std::os::unix::fs::symlink(home.join(".ssh"), &x).unwrap();
    match kept.grant(&x.join("b"), &secrets) {
        Err(SandboxError::Invalid(reason)) => {
            assert_eq!(
                reason,
                "sandbox roots must be existing canonical directories"
            )
        }
        other => panic!("{other:?}"),
    }
    match kept.recheck() {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, GRANT_CHANGED),
        other => panic!("{other:?}"),
    }
}

/// The state a second grant would leave if it judged a swapped folder from the build's
/// reads alone: the build reads the folder `x` by a walk it does not keep, `x` is swapped
/// for a link into a secret, and the walk of a root under it, read through those reads so
/// it still names the old `x`, is the only path kept beside a root outside `x`. The build
/// rechecks clean before that walk is kept, and the install's recheck alone refuses it
/// after.
#[cfg(unix)]
#[test]
fn a_stale_walk_kept_from_the_build_reads_is_refused_at_install() {
    let (folder, home, secrets, x) = two_roots_under_one_folder();
    let other = canonical(folder.path()).unwrap().join("other");
    std::fs::create_dir_all(&other).unwrap();
    let mut kept = granted_build(&home, &secrets, &other);
    let seed = Walk::read(&x.join("a"), &mut kept.walked.reads).unwrap();
    let at = x.components().count() - 1;
    std::fs::rename(&x, folder.path().join("x-old")).unwrap();
    std::fs::create_dir_all(home.join(".ssh").join("b")).unwrap();
    std::os::unix::fs::symlink(home.join(".ssh"), &x).unwrap();
    assert!(kept.recheck().is_ok());
    let stale = Walk::read(&x.join("b"), &mut kept.walked.reads).unwrap();
    assert_eq!(stale.ids[at], seed.ids[at]);
    kept.keep(stale, &secrets);
    assert_eq!(kept.checked.len(), 2);
    match kept.recheck() {
        Err(SandboxError::Invalid(reason)) => assert!(
            reason == GRANT_CHANGED || reason == SECRET_ROOT_REFUSED,
            "{reason}"
        ),
        other => panic!("{other:?}"),
    }
}

/// The path spelled with its last separator doubled: equal to it as a [`Path`], different
/// in its bytes. Input: the path. Output: the other spelling.
#[cfg(unix)]
fn doubled(path: &Path) -> PathBuf {
    let mut text = path.parent().unwrap().as_os_str().to_owned();
    text.push("//");
    text.push(path.file_name().unwrap());
    let other = PathBuf::from(text);
    assert_ne!(other.as_os_str(), path.as_os_str());
    assert_eq!(other, path);
    other
}

/// A second grant whose list spells one secret with a doubled slash walks no second copy of
/// it, and a grant whose list has the same bytes as the walked ones makes no [`Path`]
/// compare in the build's secret walks.
#[cfg(unix)]
#[test]
fn a_secret_spelled_with_a_doubled_slash_is_walked_once_per_build() {
    let (_folder, home, secrets, x) = two_roots_under_one_folder();
    let mut kept = granted_build(&home, &secrets, &x.join("a"));
    let walked = kept.walked.secrets.len();
    let mut respelled = secrets.clone();
    respelled[0] = doubled(&secrets[0]);
    kept.grant(&x.join("b"), &respelled).unwrap();
    assert_eq!(kept.walked.secrets.len(), walked);
    std::fs::create_dir_all(x.join("c")).unwrap();
    let before = path_fallbacks();
    kept.grant(&x.join("c"), &secrets).unwrap();
    assert_eq!(path_fallbacks() - before, 0);
}

/// A root granted again with one secret spelled with a doubled slash keeps one entry for
/// that secret, and granted again with the same bytes makes no [`Path`] compare.
#[cfg(unix)]
#[test]
fn a_root_granted_again_with_a_respelled_secret_keeps_one_entry_for_it() {
    let (_folder, home, secrets, x) = two_roots_under_one_folder();
    let mut kept = granted_build(&home, &secrets, &x.join("a"));
    let against = kept.checked[0].against.len();
    let mut respelled = secrets.clone();
    respelled[0] = doubled(&secrets[0]);
    kept.grant(&x.join("a"), &respelled).unwrap();
    assert_eq!(kept.checked.len(), 1);
    assert_eq!(kept.checked[0].against.len(), against);
    let before = path_fallbacks();
    kept.grant(&x.join("a"), &secrets).unwrap();
    assert_eq!(path_fallbacks() - before, 0);
}

/// A secret one grant names with a doubled slash and another names plainly is judged at
/// install for both: made a link to the second grant's root after the build, the install
/// refuses. A build whose lists all have the same bytes rechecks with no [`Path`] compare.
#[cfg(unix)]
#[test]
fn a_secret_respelled_in_a_second_grant_is_still_judged_at_install() {
    let (folder, home, secrets, x) = two_roots_under_one_folder();
    let plain = granted_build(&home, &secrets, &x.join("a"));
    let before = path_fallbacks();
    assert!(plain.recheck().is_ok());
    assert_eq!(path_fallbacks() - before, 0);
    let secret = canonical(folder.path()).unwrap().join("s");
    std::fs::create_dir_all(&secret).unwrap();
    let mut first = secrets.clone();
    first.push(secret.clone());
    let mut second = secrets.clone();
    second.push(doubled(&secret));
    let mut kept = granted_build(&home, &first, &x.join("a"));
    kept.grant(&x.join("b"), &second).unwrap();
    assert!(kept.recheck().is_ok());
    std::fs::remove_dir(&secret).unwrap();
    std::os::unix::fs::symlink(x.join("b"), &secret).unwrap();
    match kept.recheck() {
        Err(SandboxError::Invalid(reason)) => assert_eq!(reason, SECRET_ROOT_REFUSED),
        other => panic!("{other:?}"),
    }
}
