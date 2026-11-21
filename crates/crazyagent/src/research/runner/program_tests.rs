use super::{git_safe, windows_no_program, windows_value};
use std::path::{Path, PathBuf};

/// The keys of every engine and IDE git setting that names a hook folder or a program git
/// would start, each with the table it sits in, in table order.
const PROGRAM_KEYS: [(&str, &str); 9] = [
    ("engine", "core.hooksPath"),
    ("engine", "gpg.program"),
    ("engine", "gpg.x509.program"),
    ("engine", "gpg.ssh.program"),
    ("ide", "core.sshCommand"),
    ("ide", "gpg.program"),
    ("ide", "gpg.ssh.program"),
    ("ide", "gpg.x509.program"),
    ("ide", "core.hooksPath"),
];

/// Every engine ([`git_safe`]) and IDE ([`crate::ide::git::config_env`]) setting whose key
/// is in [`PROGRAM_KEYS`], as (table, key, value) in table order, duplicates kept. Output:
/// the settings.
fn program_settings() -> Vec<(&'static str, String, String)> {
    let named = |table: &str, key: &str| PROGRAM_KEYS.contains(&(table, key));
    let mut settings = Vec::new();
    for pair in git_safe().chunks(2) {
        assert_eq!(pair[0], "-c");
        let (key, value) = pair[1].split_once('=').unwrap();
        if named("engine", key) {
            settings.push(("engine", key.to_owned(), value.to_owned()));
        }
    }
    let ide = crate::ide::git::config_env();
    let count: usize = ide[0].1.parse().unwrap();
    assert_eq!(ide[0].0, "GIT_CONFIG_COUNT");
    assert_eq!(ide.len(), 1 + 2 * count);
    for n in 0..count {
        assert_eq!(ide[1 + 2 * n].0, format!("GIT_CONFIG_KEY_{n}"));
        assert_eq!(ide[2 + 2 * n].0, format!("GIT_CONFIG_VALUE_{n}"));
        let (key, value) = (ide[1 + 2 * n].1, ide[2 + 2 * n].1);
        if named("ide", key) {
            settings.push(("ide", key.to_owned(), value.to_owned()));
        }
    }
    settings
}

/// What [`program_settings`] must read when every one of them names `value`. Input: the
/// value. Output: the settings.
fn every_program_named(value: &str) -> Vec<(&'static str, String, String)> {
    PROGRAM_KEYS
        .iter()
        .map(|(table, key)| (*table, (*key).to_owned(), value.to_owned()))
        .collect()
}

/// V244-WIN-GIT-SAFE-PROGRAM-PATH: on Unix every engine and IDE git names `/dev/null` for
/// its hook folder, its signature programs and the IDE's ssh command, a path no user can
/// plant and git cannot start, so a change back to `/usr/bin/false` or a bare name goes red.
#[cfg(unix)]
#[test]
fn engine_and_ide_git_name_dev_null_for_hooks_programs_and_ssh() {
    assert_eq!(
        program_settings(),
        every_program_named("/dev/null"),
        "a git names a program a user could plant"
    );
}

/// V244-WIN-GIT-SAFE-PROGRAM-PATH: on Windows every engine and IDE git names a missing
/// `crazyagent-none` in the folder GetSystemDirectoryW names, with forward slashes, for its
/// hook folder, its signature programs and the IDE's ssh command: only administrators can
/// create it, where any user can create `C:\dev\null\reference-transaction.exe`.
#[cfg(windows)]
#[test]
fn engine_and_ide_git_name_a_missing_system_folder_file_for_hooks_programs_and_ssh() {
    let folder = crazyagent_win_sandbox::system_folder().unwrap();
    let value = format!(
        "{}/crazyagent-none",
        folder.to_str().unwrap().replace('\\', "/")
    );
    assert_eq!(value.get(1..3), Some(":/"), "{value}");
    assert!(!std::path::Path::new(&value).exists(), "{value} exists");
    assert_eq!(
        program_settings(),
        every_program_named(&value),
        "a git names a program a user could plant"
    );
}

/// V244-WIN-GIT-SAFE-PROGRAM-PATH: the Windows value is the system folder with forward
/// slashes and `/crazyagent-none`, and a folder that is not `<letter>:\` or holds a space, a
/// shell character, an empty part or a `..` gives none, so no such value reaches a git.
#[test]
fn the_windows_value_is_a_plain_system_folder_with_forward_slashes_or_none() {
    assert_eq!(
        windows_no_program(Path::new(r"C:\Windows\system32")).as_deref(),
        Some("C:/Windows/system32/crazyagent-none")
    );
    assert_eq!(
        windows_no_program(Path::new(r"D:\WINNT\System32")).as_deref(),
        Some("D:/WINNT/System32/crazyagent-none")
    );
    for refused in [
        r"C:\My Windows\system32",
        r"C:\Win$dows\system32",
        r"C:\Windows\..\system32",
        r"C:\Windows\\system32",
        r"C:\Windows\system32\",
        r"\Windows\system32",
        r"system32",
        r"\\?\C:\Windows\system32",
        r"\\server\share\system32",
        r"C:\",
        r"C:",
        r"1:\Windows",
    ] {
        assert_eq!(windows_no_program(Path::new(refused)), None, "{refused}");
    }
}

/// #598 sec N1: a GetSystemDirectoryW error and a folder the builder refuses give the named
/// refusal `git_safe_unavailable`, never a panic that poisons the value for every later git,
/// so the engine start can refuse by name and run no git; a plain folder gives the value.
#[test]
fn an_unusable_system_folder_is_the_named_refusal_never_a_panic() {
    let error = std::io::Error::other("no system folder");
    assert_eq!(windows_value(Err(error)), Err("git_safe_unavailable"));
    for refused in [
        r"C:\My Windows\system32",
        r"\\server\share\system32",
        r"1:\Windows",
    ] {
        assert_eq!(
            windows_value(Ok(PathBuf::from(refused))),
            Err("git_safe_unavailable"),
            "{refused}"
        );
    }
    assert_eq!(
        windows_value(Ok(PathBuf::from(r"C:\Windows\system32"))).as_deref(),
        Ok("C:/Windows/system32/crazyagent-none")
    );
}
