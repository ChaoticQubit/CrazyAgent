use super::{Identity, PinnedBootstrap, Root};
use crate::platform::FileExt;
use crate::protocol::{Cursor, PlanId, RequestId, TaskId};
use crate::worker_dispatch::{AttemptRef, RunnerBootstrap};
use crazyagent_win_sandbox::private;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const REFUSED: &str = "owner_capability_replaced_or_unsafe";
const PRESERVED: &str = "owner_cleanup_replaced_artifact_preserved";

/// A private owner root in a fresh temp folder spelled as Windows names it, opened as the
/// engine opens one. Output: the temp folder, kept alive by the caller, the root path and
/// the held root.
fn owner_root() -> (tempfile::TempDir, PathBuf, Root) {
    let temp = tempfile::tempdir().unwrap();
    let full = std::fs::canonicalize(temp.path()).unwrap();
    let base = PathBuf::from(full.to_str().unwrap().strip_prefix(r"\\?\").unwrap());
    let path = base.join("root");
    drop(private::dir(&path).unwrap());
    let root = Root::open(&path).unwrap();
    (temp, path, root)
}

/// Makes a cap file as the engine does and writes `bytes` into it. Inputs: the root, the
/// name and the content. Output: the held file and its identity.
fn made_key(root: &Root, name: &str, bytes: &[u8]) -> (File, Identity) {
    let file = root
        .open_key(name, true)
        .expect("the engine makes a cap file");
    file.write_all_at(bytes, 0).unwrap();
    file.sync_all().unwrap();
    let identity = Identity::of_file(&file).unwrap();
    (file, identity)
}

/// A private 32-byte file that carries no label, made by W1 as any private store file is.
/// Inputs: the path. Output: none.
fn plain_private(path: &Path) {
    let file = private::file(path, true).unwrap();
    file.write_all_at(&[3; 32], 0).unwrap();
}

/// Runs one Windows command and requires it to succeed. Inputs: the program and its
/// arguments. Output: none; a failure panics with the command's error output.
fn run(program: &str, args: &[&std::ffi::OsStr]) {
    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped());
    let done = kernel_processes::spawn_gate::output(&mut command).unwrap();
    assert!(
        done.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&done.stderr)
    );
}

/// The refusal code a cap-file call returned. Input: the result. Output: its code text.
fn refusal<T>(result: Result<T, crate::engine::EngineError>) -> String {
    match result {
        Ok(_) => panic!("the cap file was accepted"),
        Err(error) => error.0,
    }
}

#[test]
fn a_cap_file_round_trips_while_the_engine_holds_it() {
    let (_temp, path, root) = owner_root();
    let (engine, identity) = made_key(&root, "owner-tui.cap", &[9; 32]);
    root.verify_key("owner-tui.cap", &engine, identity)
        .expect("the engine's own cap file verifies");
    let client = Root::open(&path).unwrap();
    let read = client
        .open_key("owner-tui.cap", false)
        .expect("a client opens the cap file while the engine holds it");
    let seen = Identity::of_file(&read).unwrap();
    assert!(seen == identity);
    client.verify_key("owner-tui.cap", &read, seen).unwrap();
    let mut token = crate::engine::secret_frame::owner_token();
    read.read_exact_at(&mut **token, 0).unwrap();
    assert_eq!(**token, [9; 32]);
    assert!(
        std::fs::rename(path.join("owner-tui.cap"), path.join("moved.cap")).is_err(),
        "a held cap file is never renamed"
    );
    drop((engine, read));
    std::fs::rename(path.join("owner-tui.cap"), path.join("moved.cap"))
        .expect("the same rename works once no handle holds the cap file");
}

#[test]
fn a_hard_linked_cap_file_is_refused() {
    let (_temp, path, root) = owner_root();
    plain_private(&path.join("target"));
    std::fs::hard_link(path.join("target"), path.join("owner-tui.cap")).unwrap();
    assert_eq!(refusal(root.open_key("owner-tui.cap", false)), REFUSED);
}

#[test]
fn a_symbolic_link_cap_file_is_refused() {
    let (_temp, path, root) = owner_root();
    plain_private(&path.join("target"));
    std::os::windows::fs::symlink_file(path.join("target"), path.join("owner-tui.cap")).unwrap();
    assert_eq!(refusal(root.open_key("owner-tui.cap", false)), REFUSED);
}

#[test]
fn a_cap_file_with_a_foreign_entry_is_refused() {
    let (_temp, path, root) = owner_root();
    drop(made_key(&root, "owner-tui.cap", &[9; 32]));
    run(
        "icacls",
        &[
            path.join("owner-tui.cap").as_os_str(),
            "/grant".as_ref(),
            "*S-1-1-0:(R)".as_ref(),
        ],
    );
    assert_eq!(refusal(root.open_key("owner-tui.cap", false)), REFUSED);
}

#[test]
fn a_swapped_unlabelled_or_short_cap_file_is_refused() {
    let (_temp, path, root) = owner_root();
    let (engine, identity) = made_key(&root, "owner-tui.cap", &[9; 32]);
    let (other, other_identity) = made_key(&root, "other.cap", &[9; 32]);
    assert_eq!(
        refusal(root.verify_key("owner-tui.cap", &other, other_identity)),
        REFUSED,
        "another private 32-byte cap file is not the one the name holds"
    );
    assert_eq!(
        refusal(root.verify_key("owner-tui.cap", &engine, other_identity)),
        REFUSED,
        "the file id recorded at create is compared"
    );
    root.verify_key("owner-tui.cap", &engine, identity).unwrap();
    plain_private(&path.join("plain.cap"));
    let plain = root.open_key("plain.cap", false).unwrap();
    let plain_identity = Identity::of_file(&plain).unwrap();
    assert_eq!(
        refusal(root.verify_key("plain.cap", &plain, plain_identity)),
        REFUSED,
        "a cap file without the medium no-read-up label is refused"
    );
    let (short, short_identity) = made_key(&root, "short.cap", &[9; 31]);
    assert_eq!(
        refusal(root.verify_key("short.cap", &short, short_identity)),
        REFUSED
    );
}

#[test]
fn unlink_owned_deletes_only_the_held_entry_it_names() {
    let (_temp, path, root) = owner_root();
    let (a, a_identity) = made_key(&root, "a.cap", &[1; 32]);
    let (b, b_identity) = made_key(&root, "b.cap", &[2; 32]);
    assert_eq!(
        refusal(root.unlink_owned("b.cap", a_identity, false, Some(&a))),
        PRESERVED
    );
    assert_eq!(
        refusal(root.unlink_owned("a.cap", b_identity, false, Some(&a))),
        PRESERVED
    );
    assert_eq!(
        refusal(root.unlink_owned("a.cap", a_identity, false, None)),
        PRESERVED
    );
    assert!(path.join("a.cap").exists() && path.join("b.cap").exists());
    root.unlink_owned("a.cap", a_identity, false, Some(&a))
        .expect("the held entry is deleted through its own handle");
    drop(a);
    assert!(std::fs::symlink_metadata(path.join("a.cap")).is_err());
    let mut kept = [0; 32];
    b.read_exact_at(&mut kept, 0).unwrap();
    assert_eq!(kept, [2; 32]);
    root.verify_key("b.cap", &b, b_identity)
        .expect("the other cap file is untouched");
}

#[test]
fn the_runner_bootstrap_file_round_trips() {
    let (_temp, _path, root) = owner_root();
    let value = RunnerBootstrap {
        version: 1,
        registration_id: RequestId::new("runner-unit").unwrap(),
        attempt: AttemptRef {
            plan_id: PlanId::new("unit-plan").unwrap(),
            item_id: TaskId::new("unit-item").unwrap(),
            reservation_cursor: Cursor::new(10),
        },
        slot: 0,
    };
    let made = PinnedBootstrap::create(&root, value.clone()).expect("the engine writes it");
    let read = PinnedBootstrap::read(&root).expect("the runner reads it while the engine holds it");
    assert!(read.value == value);
    made.verify(&root).unwrap();
}
