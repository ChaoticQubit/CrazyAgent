use super::*;

/// Reads the user's stored PATH as written, %VAR% references unexpanded, with its registry
/// kind. Takes nothing. Returns the kind name and the value, both empty when it is unset.
#[cfg(windows)]
fn raw_user_path() -> (String, String) {
    let shown = powershell(
        "$k=Get-Item -LiteralPath 'HKCU:\\Environment'; \
if ($k.GetValueNames() -contains 'Path') { [Console]::Out.Write($k.GetValueKind('Path').ToString() + \"`n\" + \
$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)) }",
        &[],
    );
    match shown.split_once('\n') {
        Some((kind, value)) => (kind.to_owned(), value.to_owned()),
        None => (String::new(), String::new()),
    }
}

/// Stores the user's PATH back as it was: the same text with the same registry kind, or no
/// value when the kind is empty. Takes the kind name and the value. Returns nothing.
#[cfg(windows)]
fn restore_raw_user_path(kind: &str, value: &str) {
    powershell(
        "if ([string]::IsNullOrEmpty($env:CRAZYAGENT_TEST_KIND)) { \
Remove-ItemProperty -LiteralPath 'HKCU:\\Environment' -Name 'Path' -ErrorAction SilentlyContinue } else { \
Set-ItemProperty -LiteralPath 'HKCU:\\Environment' -Name 'Path' -Value $env:CRAZYAGENT_TEST_PATH -Type $env:CRAZYAGENT_TEST_KIND }",
        &[
            ("CRAZYAGENT_TEST_KIND", kind),
            ("CRAZYAGENT_TEST_PATH", value),
        ],
    );
}

#[cfg(windows)]
#[test]
fn link_cli_adds_a_folder_named_like_powershell_code_to_the_user_path_once() {
    let _held = USER_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (kind, before) = raw_user_path();
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("cra'zy$(exit 7)");
    std::fs::create_dir(&folder).unwrap();
    let executable = folder.join("crazyagent.exe");
    let first = link_cli(&executable, root.path());
    let second = link_cli(&executable, root.path());
    let (_, after) = raw_user_path();
    restore_raw_user_path(&kind, &before);
    assert_eq!(first.unwrap(), folder);
    assert_eq!(second.unwrap(), folder);
    let shown = folder.to_str().unwrap();
    assert_eq!(after.split(';').filter(|entry| *entry == shown).count(), 1);
    assert_eq!(raw_user_path(), (kind, before));
}

#[cfg(windows)]
#[test]
fn link_cli_refuses_a_folder_holding_a_semicolon_and_leaves_the_user_path() {
    let _held = USER_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (kind, before) = raw_user_path();
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("cra;zy");
    std::fs::create_dir(&folder).unwrap();
    let linked = link_cli(&folder.join("crazyagent.exe"), root.path());
    let after = raw_user_path();
    restore_raw_user_path(&kind, &before);
    let refusal = linked.unwrap_err().to_string();
    assert!(
        refusal.contains("cra;zy") && refusal.contains("PATH is unchanged"),
        "{refusal}"
    );
    assert_eq!(after, (kind, before));
}

#[cfg(windows)]
#[test]
fn link_cli_keeps_the_user_path_kind_and_its_unexpanded_references() {
    let _held = USER_PATH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let (kind, before) = raw_user_path();
    let reference = r"%USERPROFILE%\crazyagent-kind-check";
    restore_raw_user_path("ExpandString", &format!("{reference};{before}"));
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("bin");
    std::fs::create_dir(&folder).unwrap();
    let linked = link_cli(&folder.join("crazyagent.exe"), root.path());
    let after = raw_user_path();
    restore_raw_user_path(&kind, &before);
    assert_eq!(linked.unwrap(), folder);
    assert_eq!(after.0, "ExpandString", "{after:?}");
    assert!(after.1.starts_with(&format!("{reference};")), "{after:?}");
    let shown = folder.to_str().unwrap();
    assert_eq!(
        after.1.split(';').filter(|entry| *entry == shown).count(),
        1,
        "{after:?}"
    );
    assert_eq!(raw_user_path(), (kind, before));
}
