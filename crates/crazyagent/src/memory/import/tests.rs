use super::*;
use std::os::unix::fs::{FileTypeExt, symlink};
use std::process::{Command, Stdio};
use std::time::Duration;

fn mkfifo(path: &Path) {
    let output = crate::spawn_gate::output(
        Command::new("/usr/bin/mkfifo")
            .arg(path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )
    .unwrap();
    assert!(
        output.status.success(),
        "mkfifo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        std::fs::symlink_metadata(path)
            .unwrap()
            .file_type()
            .is_fifo()
    );
}

#[test]
fn read_folder_reads_md_files_and_skips_index_links_and_large() {
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target.md");
    std::fs::write(&target, "Linked memory text.").unwrap();
    let folder = tempfile::tempdir().unwrap();
    let path = folder.path();
    std::fs::write(
        path.join("a.md"),
        "---\ntitle: Meals\ntags: [food]\n---\n\n  Plan meals on Sunday.\r\nShop on Monday.\n\n",
    )
    .unwrap();
    std::fs::write(path.join("MEMORY.md"), "- [a](a.md) index").unwrap();
    std::fs::write(path.join("notes.txt"), "Not markdown.").unwrap();
    std::fs::write(path.join(".hidden.md"), "Hidden memory.").unwrap();
    symlink(&target, path.join("link.md")).unwrap();
    mkfifo(&path.join("b.md"));
    std::fs::write(path.join("big.md"), "b".repeat(70 * 1024)).unwrap();
    std::fs::write(path.join("bad.md"), [b'o', b'k', 0xff, 0xfe]).unwrap();
    std::fs::write(path.join("long.md"), "l".repeat(5000)).unwrap();

    let folder_path = path.to_path_buf();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::spawn(move || done.send(read_folder(&folder_path)).unwrap());
    let scan = finished
        .recv_timeout(Duration::from_secs(10))
        .expect("a FIFO named .md must not block the folder read")
        .unwrap();

    assert_eq!(
        scan.files,
        vec![ImportFile {
            file: "a.md".into(),
            text: "Plan meals on Sunday.\nShop on Monday.".into(),
        }]
    );
    assert_eq!(scan.skipped, 5, "link, fifo, big, bad and long");
}

#[test]
fn read_folder_refuses_relative_missing_and_linked_folder() {
    let real = tempfile::tempdir().unwrap();
    std::fs::write(real.path().join("a.md"), "A memory.").unwrap();
    let links = tempfile::tempdir().unwrap();
    let linked = links.path().join("linked");
    symlink(real.path(), &linked).unwrap();
    let file = real.path().join("a.md");
    let missing = real.path().join("missing");
    for folder in [
        Path::new("notes"),
        missing.as_path(),
        linked.as_path(),
        file.as_path(),
    ] {
        assert_eq!(
            read_folder(folder).unwrap_err().0,
            "memory_import_folder_invalid",
            "{}",
            folder.display()
        );
    }
    assert_eq!(read_folder(real.path()).unwrap().files.len(), 1);
}

#[test]
fn without_frontmatter_drops_only_a_complete_leading_block() {
    for (text, body) in [
        ("Plain memory.", "Plain memory."),
        ("---\n---\nAfter an empty block.", "After an empty block."),
        ("---\nx", "---\nx"),
        ("---\nx\n---", ""),
        ("---\ntitle: a\n---\nBody\n", "Body\n"),
        ("Text\n---\nkept: yes\n---\n", "Text\n---\nkept: yes\n---\n"),
    ] {
        assert_eq!(without_frontmatter(text), body, "{text:?}");
    }
}
