use super::{RegisteredNativeAuthHost, pinnable_at};
use crate::engine::EngineError;
use crate::harnesses::finding::{AgentSpace, PATH_AGENT_SPACE, check_file};
use crazyagent_win_sandbox::test_files::{null_dacl, supersede, writable_view};
use sha2::Digest;
use std::fs::{File, OpenOptions};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

const FILE_WRITE_ATTRIBUTES: u32 = 0x100;
const FILE_SHARE_READ: u32 = 0x1;
const FILE_SHARE_WRITE: u32 = 0x2;
const FILE_SHARE_DELETE: u32 = 0x4;
const UNSAFE: &str = "native_auth_host_executable_unsafe";
const CHANGED: &str = "native_auth_host_executable_changed";
const SHARING_VIOLATION: i32 = 32;
const SHARE_ALL: u32 = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
const HARNESS_BYTES: usize = 64 * 1024;

/// A private folder for one row and a harness file in it.
/// Input: the harness file name. Output: the folder guard and the harness's absolute path.
fn harness(name: &str) -> (tempfile::TempDir, PathBuf) {
    let folder = tempfile::tempdir().unwrap();
    let path = std::fs::canonicalize(folder.path()).unwrap().join(name);
    std::fs::write(&path, vec![b'A'; HARNESS_BYTES]).unwrap();
    assert!(path.is_absolute());
    (folder, plain(&path))
}

/// The plain `C:\...` spelling of a canonical path, as the one-spelling rule asks.
/// Input: the path. Output: the path without a `\\?\` prefix.
fn plain(path: &Path) -> PathBuf {
    let text = path.to_str().unwrap();
    PathBuf::from(text.strip_prefix(r"\\?\").unwrap_or(text))
}

/// Gives Everyone the rights an `icacls` grant names on a file or folder.
/// Inputs: the path and the grant, such as `(W)` or `(DC)`. Output: none; panics on failure.
fn grant_everyone(path: &Path, rights: &str) {
    let granted = crate::spawn_gate::collected(
        Command::new("icacls")
            .arg(path)
            .arg("/grant")
            .arg(format!("*S-1-1-0:{rights}")),
    )
    .unwrap();
    assert!(
        granted.status.success(),
        "icacls failed: {}",
        String::from_utf8_lossy(&granted.stderr)
    );
}

/// The code an engine error carries. Input: the result. Output: the code, or "ok".
fn code<T>(result: Result<T, crate::engine::EngineError>) -> String {
    match result {
        Ok(_) => "ok".to_owned(),
        Err(error) => error.0,
    }
}

/// Opens a file again with the given access and every share mode, as another program would.
/// Inputs: the path and the access mask. Output: the open file or the OS error.
fn open_as(path: &Path, access: u32) -> std::io::Result<File> {
    OpenOptions::new()
        .access_mode(access)
        .share_mode(SHARE_ALL)
        .open(path)
}

/// A harness the user owns, with no other writer on it or its folder, is pinned and its hash is
/// the file's SHA-256; an Everyone write entry or a NULL access list is refused as unsafe.
#[test]
fn a_private_harness_is_pinned_and_an_everyone_write_entry_or_a_null_dacl_is_refused() {
    let (_folder, path) = harness("codex.exe");
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    let expected = sha2::Sha256::digest(std::fs::read(&path).unwrap());
    let expected: String = expected.iter().map(|byte| format!("{byte:02x}")).collect();
    assert_eq!(host.verified_sha256().unwrap(), expected);
    let (_open, written) = harness("codex.exe");
    grant_everyone(&written, "(W)");
    assert_eq!(
        code(RegisteredNativeAuthHost::capture_pinned(&written)),
        UNSAFE
    );
    let (_null, nulled) = harness("codex.exe");
    null_dacl(&nulled).unwrap();
    assert_eq!(
        code(RegisteredNativeAuthHost::capture_pinned(&nulled)),
        UNSAFE
    );
}

/// The harness's folder chain is judged as harness search judges it
/// (`service::changeable_by_others`): Everyone with delete-child or add-file on the harness's
/// folder is refused, since either lets another account swap the harness or plant a DLL
/// beside it; the same harness in a private folder passes.
#[test]
fn a_harness_folder_others_can_delete_or_add_files_in_is_refused() {
    let (_private, path) = harness("claude.exe");
    assert_eq!(code(RegisteredNativeAuthHost::capture_pinned(&path)), "ok");
    assert_eq!(code(pinnable_at(&path)), "ok");
    for grant in ["(DC)", "(WD)"] {
        let (_folder, opened) = harness("claude.exe");
        grant_everyone(opened.parent().unwrap(), grant);
        assert_eq!(
            code(RegisteredNativeAuthHost::capture_pinned(&opened)),
            UNSAFE,
            "{grant}"
        );
        assert_eq!(code(pinnable_at(&opened)), UNSAFE, "{grant}");
    }
}

/// While pinned no program opens the harness for write (sharing violation) and its bytes stay;
/// a capture while another handle holds write access, or while a writable mapped view made
/// before it lives, is refused as changed (probe 2c).
#[test]
fn a_pinned_harness_takes_no_write_and_a_harness_open_for_write_is_never_pinned() {
    let (_folder, path) = harness("codex.exe");
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    let before = host.verified_sha256().unwrap();
    let refused = OpenOptions::new()
        .write(true)
        .share_mode(SHARE_ALL)
        .open(&path);
    assert_eq!(refused.unwrap_err().raw_os_error(), Some(SHARING_VIOLATION));
    assert_eq!(host.verified_sha256().unwrap(), before);
    drop(host);
    let writer = OpenOptions::new()
        .write(true)
        .share_mode(SHARE_ALL)
        .open(&path)
        .unwrap();
    assert_eq!(
        code(RegisteredNativeAuthHost::capture_pinned(&path)),
        CHANGED
    );
    drop(writer);
    let view = writable_view(&path).unwrap();
    assert_eq!(
        code(RegisteredNativeAuthHost::capture_pinned(&path)),
        CHANGED
    );
    drop(view);
    assert_eq!(code(RegisteredNativeAuthHost::capture_pinned(&path)), "ok");
}

/// Probe 2d1 as a row: a supersede with DELETE access alone succeeds under the pin and empties
/// the same file id, so the next pre-spawn check refuses the harness as changed.
#[test]
fn a_supersede_through_delete_sharing_is_refused_at_the_next_check() {
    let (_folder, path) = harness("claude.exe");
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    assert_eq!(code(host.verified_path()), "ok");
    supersede(&path).unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    assert_eq!(code(host.verified_path()), CHANGED);
    assert_eq!(code(host.verified_sha256()), CHANGED);
}

/// B: every pre-spawn check hashes the pinned file again on Windows, since no time on Windows
/// proves its bytes; three checks are three hashes, and a time change alone keeps the pin.
#[test]
fn every_check_hashes_the_pinned_harness_again_and_a_time_change_alone_keeps_it() {
    let (_folder, path) = harness("codex.exe");
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    let digest = host.verified_sha256().unwrap();
    let after_first = host.rehashes();
    assert_eq!(code(host.verified_path()), "ok");
    assert_eq!(code(host.verified_path()), "ok");
    assert_eq!(host.rehashes(), after_first + 2);
    let touch = open_as(&path, FILE_WRITE_ATTRIBUTES).unwrap();
    touch
        .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(86_400))
        .unwrap();
    drop(touch);
    assert_eq!(host.verified_sha256().unwrap(), digest);
}

/// rv-sec2 #721 r1 5b: a hard link made after capture changes the link count, which a retime
/// never adopts, so the next check refuses the harness as changed.
#[test]
fn a_hard_link_made_after_capture_is_refused_not_adopted() {
    let (_folder, path) = harness("codex.exe");
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    assert_eq!(code(host.verified_path()), "ok");
    std::fs::hard_link(&path, path.with_file_name("second.exe")).unwrap();
    assert_eq!(code(host.verified_path()), CHANGED);
}

/// rv-sec2 #721 r2 C5: the time one pre-spawn check takes on this image for a harness of the
/// installed Claude Code's size (360 MB), written to the file the farm names, never asserted.
#[test]
#[ignore = "farm timing probe, run with --ignored"]
fn the_time_one_check_takes_for_a_360_mb_harness() {
    let out = std::env::var_os("CRAZYAGENT_FARM_PROBE_OUT").unwrap();
    let (_folder, path) = harness("claude.exe");
    std::fs::write(&path, vec![0x5a; 360 * 1024 * 1024]).unwrap();
    let captured = std::time::Instant::now();
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    let capture = captured.elapsed();
    let mut checks = Vec::new();
    for _ in 0..3 {
        let started = std::time::Instant::now();
        host.verified_path().unwrap();
        checks.push(started.elapsed().as_millis());
    }
    std::fs::write(
        out,
        format!("capture_ms={} check_ms={checks:?}\n", capture.as_millis()),
    )
    .unwrap();
}

/// A second name (a hard link) or a symbolic link at the path is refused; the plain file passes.
#[test]
fn a_hard_linked_harness_or_a_link_at_the_path_is_refused() {
    let (_folder, path) = harness("codex.exe");
    assert_eq!(code(RegisteredNativeAuthHost::capture_pinned(&path)), "ok");
    let second = path.with_file_name("second.exe");
    std::fs::hard_link(&path, &second).unwrap();
    assert_eq!(
        code(RegisteredNativeAuthHost::capture_pinned(&path)),
        UNSAFE
    );
    std::fs::remove_file(&second).unwrap();
    let link = path.with_file_name("link.exe");
    std::os::windows::fs::symlink_file(&path, &link).unwrap();
    assert_eq!(
        code(RegisteredNativeAuthHost::capture_pinned(&link)),
        UNSAFE
    );
    assert_eq!(code(RegisteredNativeAuthHost::capture_pinned(&path)), "ok");
}

/// An updater's swap at the path: a rename aside with a new file, and a replace over the pinned
/// path, leave another file at the path, refused as changed; the new file pins on its own.
#[test]
fn a_harness_swapped_aside_or_replaced_over_is_refused_and_the_new_file_pins() {
    let (_folder, path) = harness("claude.exe");
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    std::fs::rename(&path, path.with_extension("old")).unwrap();
    std::fs::write(&path, vec![b'D'; HARNESS_BYTES]).unwrap();
    assert_eq!(code(host.verified_path()), CHANGED);
    let fresh = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    let replacement = path.with_extension("new");
    std::fs::write(&replacement, vec![b'E'; HARNESS_BYTES]).unwrap();
    match std::fs::rename(&replacement, &path) {
        Ok(()) => assert_eq!(code(fresh.verified_path()), CHANGED),
        Err(error) => {
            assert!(error.raw_os_error().is_some());
            assert_eq!(code(fresh.verified_path()), "ok");
        }
    }
}

/// Q2 row 4c: while pinned, the harness's own folder cannot be renamed and a new folder put in
/// its place, and the pin still verifies.
#[test]
fn the_folder_of_a_pinned_harness_cannot_be_renamed_while_pinned() {
    let (_folder, path) = harness("codex.exe");
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    let parent = path.parent().unwrap().to_path_buf();
    let moved = parent.with_file_name(format!(
        "{}-moved",
        parent.file_name().unwrap().to_str().unwrap()
    ));
    assert!(std::fs::rename(&parent, &moved).is_err());
    assert_eq!(code(host.verified_path()), "ok");
}

/// Length bounds: an empty file and one past 1 GiB are refused; a 1-byte file passes.
#[test]
fn an_empty_or_over_long_harness_is_refused_and_one_byte_passes() {
    let (_folder, path) = harness("codex.exe");
    std::fs::write(&path, b"").unwrap();
    assert_eq!(
        code(RegisteredNativeAuthHost::capture_pinned(&path)),
        UNSAFE
    );
    File::create(&path)
        .unwrap()
        .set_len(1024 * 1024 * 1024 + 1)
        .unwrap();
    assert_eq!(
        code(RegisteredNativeAuthHost::capture_pinned(&path)),
        UNSAFE
    );
    std::fs::write(&path, b"A").unwrap();
    assert_eq!(code(RegisteredNativeAuthHost::capture_pinned(&path)), "ok");
}

/// Q3 row 6b: a batch file is never a harness, whatever the case of its extension.
#[test]
fn a_batch_file_harness_is_refused_and_an_exe_passes() {
    for name in ["codex.bat", "codex.cmd", "codex.BAT", "codex.Cmd"] {
        let (_folder, path) = harness(name);
        assert_eq!(
            code(RegisteredNativeAuthHost::capture_pinned(&path)),
            UNSAFE,
            "{name}"
        );
    }
    let (_folder, path) = harness("codex.exe");
    assert_eq!(code(RegisteredNativeAuthHost::capture_pinned(&path)), "ok");
}

/// Q5: with the custodian on Windows (N3 r2) the live pin is on there, so harness search and
/// every engine-start capture pin as the pin itself does.
#[test]
fn the_live_pin_is_on_on_windows_with_the_custodian() {
    let (_folder, path) = harness("codex.exe");
    assert_eq!(code(pinnable_at(&path)), "ok");
    assert_eq!(code(RegisteredNativeAuthHost::capture(&path)), "ok");
    assert_eq!(code(RegisteredNativeAuthHost::capture_entry(&path)), "ok");
    assert_eq!(code(RegisteredNativeAuthHost::capture_pinned(&path)), "ok");
}

/// The `.cmd` npm 11's cmd-shim 8.0.0 writes for Codex, the text of tools/batch.rs's row
/// fixture, naming [`CODEX_SCRIPT`] below the shim's folder.
const CODEX_SHIM: &str = "@ECHO off\r\nGOTO start\r\n:find_dp0\r\nSET dp0=%~dp0\r\nEXIT /b\r\n:start\r\nSETLOCAL\r\nCALL :find_dp0\r\n\r\nIF EXIST \"%dp0%\\node.exe\" (\r\n  SET \"_prog=%dp0%\\node.exe\"\r\n) ELSE (\r\n  SET \"_prog=node\"\r\n  SET PATHEXT=%PATHEXT:;.JS;=;%\r\n)\r\n\r\nendLocal & goto #_undefined_# 2>NUL || title %COMSPEC% & \"%_prog%\"  \"%dp0%\\node_modules\\@openai\\codex\\bin\\codex.js\" %*\r\n";
/// The script [`CODEX_SHIM`] starts, below the shim's folder.
const CODEX_SCRIPT: &str = r"node_modules\@openai\codex\bin\codex.js";

/// A folder where an owner's npm install would be: below LOCALAPPDATA, private to this
/// account and outside the temp folders the agent space holds. Output: the folder, removed
/// when dropped, and its real path.
fn owner_folder() -> (tempfile::TempDir, PathBuf) {
    let local = std::env::var_os("LOCALAPPDATA").expect("Windows sets LOCALAPPDATA");
    let folder = tempfile::Builder::new()
        .prefix("n3-rows-")
        .tempdir_in(local)
        .unwrap();
    let real = std::fs::canonicalize(folder.path()).unwrap();
    (folder, real)
}

/// An npm install of Codex in `folder`: `codex.cmd` and the script it starts. Input: the
/// folder's real path. Output: the shim in the plain spelling, and the script's real path.
fn npm_codex(folder: &Path) -> (PathBuf, PathBuf) {
    let script = folder.join(CODEX_SCRIPT);
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, vec![b'A'; HARNESS_BYTES]).unwrap();
    let shim = folder.join("codex.cmd");
    std::fs::write(&shim, CODEX_SHIM).unwrap();
    (plain(&shim), std::fs::canonicalize(&script).unwrap())
}

/// The engine-start check harness search and a re-pin run on an entry, for an engine folder
/// with no database yet. Input: the engine folder. Output: the check.
fn start_check(engine: &Path) -> impl Fn(&Path) -> Result<(), EngineError> + '_ {
    move |path| {
        let space = AgentSpace::open(engine)?;
        check_file(path, &space, None).map(|_| ())
    }
}

/// rv-sec2 #741: harness search judges an npm shim by the script it starts, so a PATH-found
/// `codex.cmd` passes the pin rule rather than being refused as a batch file.
#[test]
fn an_npm_codex_shim_passes_harness_search_by_the_script_it_starts() {
    let (_owner, folder) = owner_folder();
    let engine = tempfile::tempdir().unwrap();
    let (shim, _script) = npm_codex(&folder);
    assert_eq!(code(start_check(engine.path())(&shim)), "ok");
}

/// A shim outside the agent space whose script lies in a `.crazyagent` folder is refused by
/// harness search as agent space, as a link into one is on Unix, since engine start and the
/// catalog pin the script with no second check.
#[test]
fn an_npm_shim_whose_script_is_in_agent_space_is_refused_by_harness_search() {
    let (_owner, folder) = owner_folder();
    let engine = tempfile::tempdir().unwrap();
    let script = folder
        .join("node_modules")
        .join(".crazyagent")
        .join("codex.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, vec![b'A'; HARNESS_BYTES]).unwrap();
    let shim = folder.join("codex.cmd");
    std::fs::write(
        &shim,
        CODEX_SHIM.replace(
            r"node_modules\@openai\codex\bin\codex.js",
            r"node_modules\.crazyagent\codex.js",
        ),
    )
    .unwrap();
    assert_eq!(
        crate::paths::executable_file(&shim),
        Some(std::fs::canonicalize(&script).unwrap())
    );
    assert_eq!(
        code(start_check(engine.path())(&plain(&shim))),
        PATH_AGENT_SPACE
    );
}

/// rv-sec2 #741: an npm shim pins the script it starts, and `current` resolves the entry as
/// capture does, so the same host comes back rather than a re-pin of the `.cmd`.
#[test]
fn an_npm_codex_shim_pins_its_script_and_current_keeps_the_same_host() {
    let (_owner, folder) = owner_folder();
    let engine = tempfile::tempdir().unwrap();
    let (shim, script) = npm_codex(&folder);
    let host = Arc::new(RegisteredNativeAuthHost::capture_entry(&shim).unwrap());
    assert_eq!(host.verified_path().unwrap(), script);
    let now = host.current(start_check(engine.path())).unwrap();
    assert!(Arc::ptr_eq(&host, &now));
}

/// rv-sec2 #741: the script of a pinned npm shim rewritten in place is refused through
/// `current`, never re-pinned: the held pin lets no write in, so the one rewrite Windows
/// allows is a supersede that empties the file, which the pin's rule refuses.
#[test]
fn a_rewritten_npm_script_is_refused_through_current() {
    let (_owner, folder) = owner_folder();
    let engine = tempfile::tempdir().unwrap();
    let (shim, script) = npm_codex(&folder);
    let host = Arc::new(RegisteredNativeAuthHost::capture_entry(&shim).unwrap());
    assert!(Arc::ptr_eq(
        &host,
        &host.current(start_check(engine.path())).unwrap()
    ));
    supersede(&plain(&script)).unwrap();
    assert_eq!(std::fs::metadata(&script).unwrap().len(), 0);
    assert_eq!(code(host.current(start_check(engine.path()))), UNSAFE);
}

/// An npm shim that now names another script, as an npm update may write it, re-pins that
/// script through `current` with the engine-start check, so an update is not refused.
#[test]
fn an_npm_shim_that_names_another_script_re_pins_it_through_current() {
    let (_owner, folder) = owner_folder();
    let engine = tempfile::tempdir().unwrap();
    let (shim, script) = npm_codex(&folder);
    let host = Arc::new(RegisteredNativeAuthHost::capture_entry(&shim).unwrap());
    let moved = script.with_file_name("codex-next.js");
    std::fs::write(&moved, vec![b'B'; HARNESS_BYTES]).unwrap();
    std::fs::write(
        &shim,
        CODEX_SHIM.replace(r"bin\codex.js", r"bin\codex-next.js"),
    )
    .unwrap();
    let now = host.current(start_check(engine.path())).unwrap();
    assert!(!Arc::ptr_eq(&host, &now));
    assert_eq!(
        now.verified_path().unwrap(),
        std::fs::canonicalize(&moved).unwrap()
    );
}

/// rv-sec2 #721 r2 C1: one `verified` call gives the path and the hash from one check, one
/// hash of the pinned harness, where `verified_path` then `verified_sha256` hash twice.
#[test]
fn one_verified_call_hashes_the_pinned_harness_once() {
    let (_folder, path) = harness("codex.exe");
    let host = RegisteredNativeAuthHost::capture_pinned(&path).unwrap();
    let before = host.rehashes();
    let (verified, sha256) = host.verified().unwrap();
    assert_eq!(host.rehashes(), before + 1);
    assert_eq!(verified, path);
    assert_eq!(
        sha256,
        format!("{:x}", sha2::Sha256::digest(vec![b'A'; HARNESS_BYTES]))
    );
}

/// Makes `link` a junction to `target`, as npm link and pnpm lay out `node_modules`. Inputs:
/// the link and the target folder. Output: none.
fn junction(link: &Path, target: &Path) {
    let made = crate::spawn_gate::collected(
        Command::new("cmd")
            .arg("/c")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdin(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(made.status.success(), "mklink /J {}", link.display());
}

/// An npm install of Codex whose package folder is a junction to `real`, the script inside
/// it. Inputs: the owner folder and the package's real folder. Output: the shim in the plain
/// spelling, the package junction's path and the script's real path.
fn npm_codex_linked(folder: &Path, real: &Path) -> (PathBuf, PathBuf, PathBuf) {
    let script = real.join("bin").join("codex.js");
    std::fs::create_dir_all(script.parent().unwrap()).unwrap();
    std::fs::write(&script, vec![b'A'; HARNESS_BYTES]).unwrap();
    let package = folder.join(r"node_modules\@openai\codex");
    std::fs::create_dir_all(package.parent().unwrap()).unwrap();
    let shim = folder.join("codex.cmd");
    std::fs::write(&shim, CODEX_SHIM).unwrap();
    (
        plain(&shim),
        package,
        std::fs::canonicalize(&script).unwrap(),
    )
}

/// rv-sec3 #752 C1: a shim whose script is reached through a junction sitting in the agent
/// space is refused by harness search, though the script's real file lies outside it, since
/// an agent could retarget that junction between the check and node opening the script.
#[test]
fn an_npm_shim_reaching_its_script_through_a_junction_in_agent_space_is_refused_by_search() {
    let (_owner, folder) = owner_folder();
    let engine = tempfile::tempdir().unwrap();
    let real = folder.join("real-codex");
    let (shim, package, script) = npm_codex_linked(&folder, &real);
    let hop = folder.join(".crazyagent").join("hop");
    std::fs::create_dir_all(hop.parent().unwrap()).unwrap();
    junction(&hop, &real);
    junction(&package, &hop);
    assert_eq!(crate::paths::executable_file(&shim), Some(script.clone()));
    assert!(!AgentSpace::open(engine.path()).unwrap().holds(&script));
    assert_eq!(code(start_check(engine.path())(&shim)), PATH_AGENT_SPACE);
}

/// rv-sec3 #752 C1: a pinned npm shim whose package junction is moved after the pin to pass
/// through a junction in the agent space is refused through `current`, even when the chain
/// still ends at the pinned script, so the hop an agent holds is never left to the launch.
#[test]
fn a_junction_into_agent_space_placed_after_the_pin_is_refused_through_current() {
    let (_owner, folder) = owner_folder();
    let engine = tempfile::tempdir().unwrap();
    let real = folder.join("real-codex");
    let (shim, package, script) = npm_codex_linked(&folder, &real);
    junction(&package, &real);
    assert_eq!(code(start_check(engine.path())(&shim)), "ok");
    let host = Arc::new(RegisteredNativeAuthHost::capture_entry(&shim).unwrap());
    assert_eq!(host.verified_path().unwrap(), script);
    std::fs::remove_dir(&package).unwrap();
    let hop = folder.join(".crazyagent").join("hop");
    std::fs::create_dir_all(hop.parent().unwrap()).unwrap();
    junction(&hop, &real);
    junction(&package, &hop);
    assert_eq!(crate::paths::executable_file(&shim), Some(script));
    assert_eq!(
        code(host.current(start_check(engine.path()))),
        PATH_AGENT_SPACE
    );
}

/// A stand-in `node.exe` in `folder`, a copy of the `where.exe` Windows ships, a real
/// program. Input: the folder, made if missing. Output: the stand-in's real path.
fn stand_in_node(folder: &Path) -> PathBuf {
    std::fs::create_dir_all(folder).unwrap();
    let shipped = PathBuf::from(std::env::var_os("SystemRoot").unwrap())
        .join("System32")
        .join("where.exe");
    let node = folder.join("node.exe");
    std::fs::copy(shipped, &node).unwrap();
    std::fs::canonicalize(node).unwrap()
}

/// The interpreter the resolver starts an npm shim with, for the places given. Inputs: the
/// shim and the folders searched in order. Output: the program word.
fn node_for(shim: &Path, folders: Vec<PathBuf>) -> std::ffi::OsString {
    let places = crate::tools::Places::new(folders, None);
    let resolved = crate::tools::resolve_entry(shim, &places).unwrap();
    resolved.argv().remove(0)
}

/// rv-sec3 #752 note 4: the `node.exe` a sign-in starts an npm shim's pinned script with is
/// never one an agent placed earlier on the search path, in a `.crazyagent` folder or
/// reached through a junction into one; the control shows the first `node.exe` found wins
/// otherwise, so the row would see an agent's copy.
#[test]
fn an_agent_placed_node_exe_earlier_on_the_path_is_not_the_one_an_npm_shim_runs() {
    let (_owner, folder) = owner_folder();
    let (shim, _script) = npm_codex(&folder.join("npm"));
    let owner_node = stand_in_node(&folder.join("nodejs"));
    let other = folder.join("other");
    let other_node = stand_in_node(&other);
    assert_eq!(
        node_for(&shim, vec![other.clone(), folder.join("nodejs")]),
        other_node.into_os_string()
    );
    let agents = folder.join(".crazyagent").join("bin");
    stand_in_node(&agents);
    assert_eq!(
        node_for(&shim, vec![agents.clone(), folder.join("nodejs")]),
        owner_node.clone().into_os_string()
    );
    let linked = folder.join("linked");
    junction(&linked, &agents);
    assert_eq!(
        node_for(&shim, vec![linked, folder.join("nodejs")]),
        owner_node.into_os_string()
    );
}
