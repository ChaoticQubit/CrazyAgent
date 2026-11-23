#![cfg(windows)]
use super::*;

/// A registry key of one row's own that stands in for the user's environment key, so no row
/// writes the PATH of the machine running it or reads another row's (V244-WIN-LINK-CLI-NEXTEST):
/// `HKCU:\Software\CrazyAgent-test-<tag>-<pid>`, removed with its values when dropped.
#[cfg(windows)]
struct ScratchKey {
    name: String,
}

#[cfg(windows)]
impl ScratchKey {
    /// Creates the key, holding a Path value of the given registry kind and text when one is
    /// given. Inputs: the row's tag, lowercase letters, and the value, none for no Path.
    /// Output: the key.
    fn new(tag: &str, path: Option<(&str, &str)>) -> Self {
        assert!(!tag.is_empty(), "a row names its key");
        assert!(tag.bytes().all(|byte| byte.is_ascii_lowercase()), "{tag}");
        let name = format!(
            r"HKCU:\Software\CrazyAgent-test-{tag}-{}",
            std::process::id()
        );
        powershell(
            "$ErrorActionPreference='Stop'; \
New-Item -Path $env:CRAZYAGENT_TEST_KEY -Force | Out-Null",
            &[("CRAZYAGENT_TEST_KEY", &name)],
        );
        let key = Self { name };
        if let Some((kind, value)) = path {
            powershell(
                "$ErrorActionPreference='Stop'; \
Set-ItemProperty -LiteralPath $env:CRAZYAGENT_TEST_KEY -Name 'Path' \
-Value $env:CRAZYAGENT_TEST_PATH -Type $env:CRAZYAGENT_TEST_KIND",
                &[
                    ("CRAZYAGENT_TEST_KEY", &key.name),
                    ("CRAZYAGENT_TEST_PATH", value),
                    ("CRAZYAGENT_TEST_KIND", kind),
                ],
            );
        }
        let (kind, value) = path.unwrap_or_default();
        assert_eq!(key.path(), (kind.to_owned(), value.to_owned()));
        key
    }

    /// Reads the key's Path value as written, %VAR% references unexpanded, with its registry
    /// kind. Output: the kind name and the value, both empty when there is no Path value.
    fn path(&self) -> (String, String) {
        let shown = powershell(
            "$ErrorActionPreference='Stop'; $k=Get-Item -LiteralPath $env:CRAZYAGENT_TEST_KEY; \
if ($k.GetValueNames() -contains 'Path') { [Console]::Out.Write( \
$k.GetValueKind('Path').ToString() + \"`n\" + \
$k.GetValue('Path','',[Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)) }",
            &[("CRAZYAGENT_TEST_KEY", &self.name)],
        );
        let (kind, value) = shown.split_once('\n').unwrap_or_default();
        assert!(["", "String", "ExpandString"].contains(&kind), "{shown}");
        (kind.to_owned(), value.to_owned())
    }
}

#[cfg(windows)]
impl Drop for ScratchKey {
    /// Removes the key and its values. A removal that fails fails the row, unless the row is
    /// already failing, where a second panic would abort the whole run.
    fn drop(&mut self) {
        let removed = crate::spawn_gate::output(&mut powershell_command(
            "$ErrorActionPreference='Stop'; \
Remove-Item -LiteralPath $env:CRAZYAGENT_TEST_KEY -Recurse",
            &[("CRAZYAGENT_TEST_KEY", &self.name)],
        ));
        let done = removed.is_ok_and(|output| output.status.success());
        assert!(done || std::thread::panicking(), "{} is left", self.name);
    }
}

#[cfg(windows)]
#[test]
fn link_cli_adds_a_folder_named_like_powershell_code_to_the_user_path_once() {
    let key = ScratchKey::new("once", None);
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("cra'zy$(exit 7)");
    std::fs::create_dir(&folder).unwrap();
    let executable = folder.join("crazyagent.exe");
    assert_eq!(link_cli_under(&executable, &key.name).unwrap(), folder);
    assert_eq!(link_cli_under(&executable, &key.name).unwrap(), folder);
    let shown = folder.to_str().unwrap();
    assert_eq!(key.path(), ("ExpandString".to_owned(), shown.to_owned()));
}

#[cfg(windows)]
#[test]
fn link_cli_refuses_a_folder_holding_a_semicolon_and_leaves_the_user_path() {
    let before = ("String", r"C:\Windows");
    let key = ScratchKey::new("semicolon", Some(before));
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("cra;zy");
    std::fs::create_dir(&folder).unwrap();
    let linked = link_cli_under(&folder.join("crazyagent.exe"), &key.name);
    let refusal = linked.unwrap_err().to_string();
    assert!(
        refusal.contains("cra;zy") && refusal.contains("PATH is unchanged"),
        "{refusal}"
    );
    assert_eq!(key.path(), (before.0.to_owned(), before.1.to_owned()));
}

#[cfg(windows)]
#[test]
fn link_cli_keeps_the_user_path_kind_and_its_unexpanded_references() {
    let before = r"%USERPROFILE%\crazyagent-kind-check;C:\Windows";
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("bin");
    std::fs::create_dir(&folder).unwrap();
    let shown = folder.to_str().unwrap();
    for kind in ["ExpandString", "String"] {
        let key = ScratchKey::new(&kind.to_ascii_lowercase(), Some((kind, before)));
        let linked = link_cli_under(&folder.join("crazyagent.exe"), &key.name);
        assert_eq!(linked.unwrap(), folder, "{kind}");
        let after = (kind.to_owned(), format!("{before};{shown}"));
        assert_eq!(key.path(), after, "{kind}");
    }
}
