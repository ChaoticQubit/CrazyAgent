use std::ffi::OsString;
use std::path::PathBuf;

/// A Codex npm cmd-shim beside its `node.exe` starts with the script in plain drive
/// spelling, since Node cannot start a main script a `\\?\` path names: the harness launch
/// words end in `C:\...\codex.js`, and only the node program word may keep the long form.
#[test]
fn a_windows_harness_script_starts_in_plain_drive_spelling() {
    let folder = tempfile::tempdir().unwrap();
    let real = folder.path().canonicalize().unwrap();
    let system = PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32");
    std::fs::copy(system.join("where.exe"), real.join("node.exe")).unwrap();
    let package = real
        .join("node_modules")
        .join("@openai")
        .join("codex")
        .join("bin");
    std::fs::create_dir_all(&package).unwrap();
    let script = package.join("codex.js");
    std::fs::write(&script, "#!/usr/bin/env node\n").unwrap();
    let shim = real.join("codex.cmd");
    std::fs::write(&shim, kernel_processes::batch::CODEX).unwrap();
    let places = crate::tools::tools_new(vec![real.clone()], None);

    let resolved = kernel_processes::finding::resolve_entry(&shim, &places).unwrap();
    let words = super::start_words(&resolved);
    let verbatim = script.canonicalize().unwrap();
    let plain = verbatim.to_str().unwrap().strip_prefix(r"\\?\").unwrap();
    assert_eq!(words.last(), Some(&OsString::from(plain)), "{words:?}");
    assert_eq!(words.len(), 2, "{words:?}");
    assert!(
        words[1..]
            .iter()
            .all(|word| !word.to_string_lossy().starts_with(r"\\?\")),
        "{words:?}"
    );
}
