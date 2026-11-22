use super::super::Root;
use super::super::master_runtime::Capture;
use super::{allocate_master_capture, master_turn_home, write_master_instructions};
use crate::engine::EngineError;
use crazyagent_win_sandbox::{dacl, private};
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// A fresh temp folder in the plain spelling every owner root is stored in. Output: the
/// temp folder, kept alive by the caller, and its plain path.
fn plain_temp() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let plain = crate::sandbox::canonical(temp.path()).unwrap();
    (temp, plain)
}

/// An owner generation root as W3 makes and holds one (PLAN-0410 C3): a W1 private folder
/// `gen` in the base, opened as a W3 [`Root`]. Input: the base. Output: the held root.
fn generation_root(base: &Path) -> Root {
    let path = base.join("gen");
    drop(private::dir(&path).unwrap());
    Root::open(&path).unwrap()
}

/// The folder of one master turn in a generation root. Inputs: the root and the turn's
/// runtime reference. Output: its path.
fn turn_folder(root: &Root, runtime_ref: &crate::protocol::RequestId) -> PathBuf {
    root.path
        .join(format!("master-runtime-{}", runtime_ref.as_str()))
}

/// The integrity label W1 reads on an open object, as a level and policy. Input: the
/// object. Output: the label, or none when it carries none.
fn label(object: &File) -> Option<(u32, u32)> {
    private::facts(object)
        .unwrap()
        .label
        .and_then(|sacl| dacl::label(&sacl).unwrap())
}

/// The medium label every W3 `open_key` secret file carries. Output: its level and policy.
fn secret_label() -> Option<(u32, u32)> {
    dacl::label(&private::secret_file_label()).unwrap()
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

/// The refusal code a turn folder step returned. Input: the result. Output: its code.
fn refusal<T>(result: Result<T, EngineError>) -> String {
    match result {
        Ok(_) => panic!("the turn folder step was accepted"),
        Err(error) => error.0,
    }
}

/// V244-WIN-MASTER S5 (PLAN-0410 D1, C3): on a W3 generation root, a master turn's folder,
/// its capture, its home and its instruction file are all W1 private. The capture carries
/// W3's medium secret label, is empty and enters [`Capture`]; the instruction file carries
/// no label, so S6 can grant the container read on it alone, and holds the text.
#[test]
fn a_windows_turn_folder_and_its_capture_home_and_instructions_are_private() {
    let (_temp, base) = plain_temp();
    let root = generation_root(&base);
    let (runtime_ref, capture_ref, capture) = allocate_master_capture(&root).unwrap();
    assert_ne!(runtime_ref, capture_ref);
    let folder = turn_folder(&root, &runtime_ref);
    private::check(&private::open_dir(&folder).unwrap()).unwrap();
    private::check(&capture).unwrap();
    assert!(secret_label().is_some());
    assert_eq!(label(&capture), secret_label());
    assert_eq!(capture.metadata().unwrap().len(), 0);
    assert!(
        folder
            .join(format!("capture-{}.jsonl", capture_ref.as_str()))
            .is_file()
    );
    let capture = Capture::new(capture, 8192).ok().unwrap();
    assert_eq!(capture.sequence(), 0);
    let home = master_turn_home(&root, &runtime_ref).unwrap();
    assert_eq!(home, folder.join("home"));
    private::check(&private::open_dir(&home).unwrap()).unwrap();
    let instructions = write_master_instructions(&root, &runtime_ref, "rules").unwrap();
    assert_eq!(instructions, folder.join("instructions.md"));
    let file = private::read_only(&instructions).unwrap();
    private::check(&file).unwrap();
    assert_ne!(label(&file), secret_label());
    assert_eq!(std::fs::read_to_string(&instructions).unwrap(), "rules");
    root.verify().unwrap();
}

/// V244-WIN-MASTER S5: a capture file enters [`Capture`] on Windows only while it is the
/// empty, W1 private, secret-labelled file the turn folder made, so one with bytes in it,
/// one opened to the Users group, or a private file without the label is refused with
/// `master_capture_identity`.
#[test]
fn a_windows_capture_with_bytes_a_users_grant_or_no_label_is_refused() {
    let (_temp, base) = plain_temp();
    let root = generation_root(&base);
    let (_, _, mut written) = allocate_master_capture(&root).unwrap();
    written.write_all(b"{}\n").unwrap();
    assert_eq!(
        Capture::new(written, 8192).err().unwrap().0,
        "master_capture_identity"
    );
    let (runtime_ref, capture_ref, granted) = allocate_master_capture(&root).unwrap();
    let path =
        turn_folder(&root, &runtime_ref).join(format!("capture-{}.jsonl", capture_ref.as_str()));
    run(
        "icacls",
        &[
            path.as_os_str(),
            "/grant".as_ref(),
            "*S-1-5-32-545:(R)".as_ref(),
        ],
    );
    assert_eq!(
        Capture::new(granted, 8192).err().unwrap().0,
        "master_capture_identity"
    );
    let plain = base.join("plain");
    drop(private::dir(&plain).unwrap());
    let unlabelled = private::file(&plain.join("capture.jsonl"), true).unwrap();
    private::check(&unlabelled).unwrap();
    assert_eq!(
        Capture::new(unlabelled, 8192).err().unwrap().0,
        "master_capture_identity"
    );
}

/// V244-WIN-MASTER S5: a Windows turn folder refuses an instruction text past the Claude
/// host's limit before writing a file, and refuses to write through a hard link planted at
/// the instruction name, so the outside file keeps its bytes. A junction planted at the
/// home's name is refused with `master_home_unsafe`, and its target stays empty.
#[test]
fn a_windows_turn_folder_refuses_large_or_planted_instructions_and_a_planted_home() {
    let (_temp, base) = plain_temp();
    let root = generation_root(&base);
    let (runtime_ref, _, _capture) = allocate_master_capture(&root).unwrap();
    let folder = turn_folder(&root, &runtime_ref);
    let large = "x".repeat(crate::engine::claude_host::INSTRUCTIONS_BYTES_MAX as usize + 1);
    assert_eq!(
        refusal(write_master_instructions(&root, &runtime_ref, &large)),
        "master_instructions_too_large"
    );
    assert!(!folder.join("instructions.md").exists());
    let outside = base.join("outside.md");
    std::fs::write(&outside, "owner").unwrap();
    std::fs::hard_link(&outside, folder.join("instructions.md")).unwrap();
    assert_eq!(
        refusal(write_master_instructions(&root, &runtime_ref, "rules")),
        "master_instructions_identity"
    );
    assert_eq!(std::fs::read_to_string(&outside).unwrap(), "owner");
    let target = base.join("target");
    std::fs::create_dir(&target).unwrap();
    run(
        "cmd",
        &[
            "/c".as_ref(),
            "mklink".as_ref(),
            "/J".as_ref(),
            folder.join("home").as_os_str(),
            target.as_os_str(),
        ],
    );
    assert!(std::fs::symlink_metadata(folder.join("home")).is_ok());
    assert_eq!(
        refusal(master_turn_home(&root, &runtime_ref)),
        "master_home_unsafe"
    );
    assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
}
