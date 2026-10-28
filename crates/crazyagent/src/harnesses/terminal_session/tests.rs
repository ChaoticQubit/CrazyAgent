use super::*;

fn unframed(parts: &[String]) -> String {
    let joined = parts.concat();
    assert!(joined.starts_with(PASTE_START), "{joined:?}");
    assert!(joined.ends_with(PASTE_END), "{joined:?}");
    joined[PASTE_START.len()..joined.len() - PASTE_END.len()].to_owned()
}

#[test]
fn a_prompt_is_one_bracketed_paste() {
    let parts = paste("Fix the build\nthen run\ttests");
    assert_eq!(
        parts,
        vec!["\x1b[200~Fix the build\nthen run\ttests\x1b[201~".to_owned()]
    );
}

#[test]
fn control_characters_cannot_end_the_paste_or_press_keys() {
    let parts = paste("a\x1b[201~\r\nb\rc\x03d\x7fe\u{9b}f\u{0}g");
    assert_eq!(unframed(&parts), "a[201~\nb\ncdefg");
    assert_eq!(parts.concat().matches(PASTE_END).count(), 1);
}

#[test]
fn a_long_prompt_is_split_into_host_inputs_on_character_boundaries() {
    let prompt = "é".repeat(INPUT_BYTES_MAX);
    let parts = paste(&prompt);
    assert!(parts.len() > 2);
    assert!(parts.iter().all(|part| part.len() <= INPUT_BYTES_MAX));
    assert_eq!(unframed(&parts), prompt);
}

#[test]
fn an_empty_prompt_is_still_one_paste() {
    assert_eq!(paste(""), vec!["\x1b[200~\x1b[201~".to_owned()]);
}

/// Makes a real FIFO at `path` with the system `mkfifo`. Input: the path. Output: none.
fn fifo(path: &std::path::Path) {
    let made = crate::spawn_gate::status(
        std::process::Command::new("/usr/bin/mkfifo")
            .arg(path)
            .env_clear()
            .stdin(std::process::Stdio::null()),
    )
    .unwrap();
    assert!(made.success());
}

/// Opens the FIFO at `path` for writing without waiting and closes it, so a reader a
/// failing test left blocked on it ends. Input: the path. Output: none.
fn release_fifo(path: &std::path::Path) {
    use std::os::unix::fs::OpenOptionsExt;
    let _ = std::fs::OpenOptions::new()
        .write(true)
        .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
        .open(path);
}

/// A private folder under the canonical `/tmp` for one key file test.
fn key_folder() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt;
    let folder = tempfile::Builder::new()
        .prefix("ca-key-")
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

/// The longest key the engine writes: a relay token.
fn longest_key() -> String {
    format!(
        "{}{}",
        crate::model_gateway::relay::TOKEN_PREFIX,
        "a".repeat(64)
    )
}

#[test]
fn dropping_a_key_file_that_still_holds_its_key_removes_it() {
    let folder = key_folder();
    let path = folder.path().join(interactive::KEY_FILE);
    std::fs::write(&path, longest_key()).unwrap();
    drop(KeyFile::new(path.clone(), longest_key().as_bytes()));
    assert!(
        !path.exists(),
        "a key file that still held its key was kept"
    );
}

#[test]
fn dropping_a_key_file_whose_path_now_holds_a_fifo_keeps_it_and_never_blocks() {
    let folder = key_folder();
    let path = folder.path().join(interactive::KEY_FILE);
    fifo(&path);
    let key = KeyFile::new(path.clone(), longest_key().as_bytes());
    let (sent, got) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        drop(key);
        let _ = sent.send(());
    });
    let dropped = got.recv_timeout(Duration::from_secs(5));
    release_fifo(&path);
    assert_eq!(
        dropped,
        Ok(()),
        "dropping a key file whose path holds a FIFO blocked for 5 s"
    );
    assert!(
        std::os::unix::fs::FileTypeExt::is_fifo(
            &std::fs::symlink_metadata(&path).unwrap().file_type()
        ),
        "the FIFO at the key path was removed"
    );
}

#[test]
#[should_panic(expected = "assertion failed: content.len() as u64 <= KEY_FILE_BYTES_MAX")]
fn a_key_file_guard_never_holds_more_bytes_than_a_key_file_is_read_back_with() {
    let folder = key_folder();
    let longer = format!("{}a", longest_key());
    let _ = KeyFile::new(folder.path().join(interactive::KEY_FILE), longer.as_bytes());
}

#[test]
fn dropping_a_key_file_whose_path_now_holds_more_than_a_key_keeps_it() {
    let folder = key_folder();
    let path = folder.path().join(interactive::KEY_FILE);
    std::fs::File::create(&path)
        .unwrap()
        .set_len(16 << 20)
        .unwrap();
    let started = Instant::now();
    drop(KeyFile::new(path.clone(), longest_key().as_bytes()));
    let took = started.elapsed();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 16 << 20);
    assert!(took < Duration::from_secs(1), "the drop took {took:?}");
}
