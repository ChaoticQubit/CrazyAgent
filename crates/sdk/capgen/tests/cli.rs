use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// Names a folder of the sdk tree. Input: its path below `crates/sdk`. Output: the folder.
fn sdk(path: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join(path)
}

/// Copies the files of one flat folder into a fresh folder under the system temp folder.
/// Inputs: the source folder and a name unique to the row. Output: the copy's folder.
fn copied(source: &Path, name: &str) -> PathBuf {
    let copy = std::env::temp_dir().join(format!("capgen-cli-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&copy);
    fs::create_dir_all(&copy).expect("the copy folder is made");
    for entry in fs::read_dir(source).expect("the source folder reads") {
        let path = entry.expect("the entry reads").path();
        assert!(path.is_file(), "{} is flat", source.display());
        fs::copy(&path, copy.join(path.file_name().expect("a file name")))
            .expect("the file copies");
    }
    copy
}

/// Runs the capgen binary over `crates/sdk/caps`. Inputs: the mode and the output folder.
/// Output: the process output.
fn capgen(mode: &str, out: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_capgen"))
        .arg(mode)
        .arg(sdk("caps"))
        .arg(out)
        .output()
        .expect("capgen runs")
}

/// Changes the first byte of one file. Input: the file. Output: nothing.
fn corrupt(path: &Path) {
    let mut bytes = fs::read(path).expect("the file reads");
    assert!(!bytes.is_empty());
    bytes[0] = if bytes[0] == b'x' { b'y' } else { b'x' };
    fs::write(path, bytes).expect("the file writes");
}

/// Checks that `--check-<target>` reports one corrupted file and `--write-<target>`
/// clears it. Inputs: the target, its checked-in folder below `crates/sdk` and the file
/// to corrupt. Output: nothing; panics on any other exit or output.
fn drift_then_write(target: &str, folder: &str, file: &str) {
    let copy = copied(&sdk(folder), target);
    corrupt(&copy.join(file));
    let check = capgen(&format!("--check-{target}"), &copy);
    assert_eq!(check.status.code(), Some(1), "{check:?}");
    assert_eq!(String::from_utf8_lossy(&check.stdout), format!("{file}\n"));
    let write = capgen(&format!("--write-{target}"), &copy);
    assert_eq!(write.status.code(), Some(0), "{write:?}");
    let again = capgen(&format!("--check-{target}"), &copy);
    assert_eq!(again.status.code(), Some(0), "{again:?}");
    assert!(again.stdout.is_empty());
    fs::remove_dir_all(&copy).expect("the copy is removed");
}

#[test]
fn check_ts_reports_drift_and_write_ts_clears_it() {
    drift_then_write("ts", "ts/src/generated", "settings.ts");
}

#[test]
fn write_ts_refuses_an_extra_file() {
    let copy = copied(&sdk("ts/src/generated"), "ts-extra");
    fs::write(copy.join("extra.ts"), "export {};\n").expect("the extra file writes");
    let write = capgen("--write-ts", &copy);
    assert_eq!(write.status.code(), Some(2), "{write:?}");
    assert!(
        String::from_utf8_lossy(&write.stderr).contains("capgen_extra_file extra.ts"),
        "{write:?}"
    );
    fs::remove_dir_all(&copy).expect("the copy is removed");
}

#[test]
fn check_wit_reports_drift_and_write_wit_clears_it() {
    drift_then_write("wit", "wit", "sdk.wit");
}

/// Runs the capgen binary's permission modes over `crates/sdk/caps` and the repository's
/// `plugins` folder. Inputs: the mode and the output folder. Output: the process output.
fn capgen_permissions(mode: &str, out: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_capgen"))
        .arg(mode)
        .arg(sdk("caps"))
        .arg(sdk("../../plugins"))
        .arg(out)
        .output()
        .expect("capgen runs")
}

#[test]
fn check_permissions_reports_drift_and_write_permissions_clears_it() {
    let copy = copied(&sdk("../kernel/plugin_host/src/generated"), "permissions");
    corrupt(&copy.join("permissions.rs"));
    let check = capgen_permissions("--check-permissions", &copy);
    assert_eq!(check.status.code(), Some(1), "{check:?}");
    assert_eq!(String::from_utf8_lossy(&check.stdout), "permissions.rs\n");
    let write = capgen_permissions("--write-permissions", &copy);
    assert_eq!(write.status.code(), Some(0), "{write:?}");
    let again = capgen_permissions("--check-permissions", &copy);
    assert_eq!(again.status.code(), Some(0), "{again:?}");
    assert!(again.stdout.is_empty());
    let short = capgen("--check-permissions", &copy);
    assert_eq!(short.status.code(), Some(2), "{short:?}");
    fs::remove_dir_all(&copy).expect("the copy is removed");
}
