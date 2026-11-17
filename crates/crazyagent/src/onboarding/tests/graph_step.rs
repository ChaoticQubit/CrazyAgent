use super::*;

#[test]
fn a_graph_mark_is_written_only_into_a_regular_file_of_its_own_and_never_waits_on_a_fifo() {
    const MARK: &str = "keep-out v1 ab\n";
    let folder = tempfile::tempdir().unwrap();
    let out = folder.path().join("fresh");
    std::fs::create_dir(&out).unwrap();
    graph::write_mark(&out, MARK).unwrap();
    assert_eq!(graph::marked(&out).as_deref(), Some(MARK));
    std::fs::write(
        out.join(graph::KEPT_OUT_MARK),
        "an older and much longer mark\n",
    )
    .unwrap();
    graph::write_mark(&out, MARK).unwrap();
    assert_eq!(
        graph::marked(&out).as_deref(),
        Some(MARK),
        "an older mark is replaced whole"
    );

    let linked = folder.path().join("linked");
    std::fs::create_dir(&linked).unwrap();
    let elsewhere = folder.path().join("owner.txt");
    std::fs::write(&elsewhere, "the owner's file\n").unwrap();
    symlink(&elsewhere, linked.join(graph::KEPT_OUT_MARK)).unwrap();
    assert!(graph::write_mark(&linked, MARK).is_err());
    std::fs::remove_file(linked.join(graph::KEPT_OUT_MARK)).unwrap();
    std::fs::hard_link(&elsewhere, linked.join(graph::KEPT_OUT_MARK)).unwrap();
    assert!(graph::write_mark(&linked, MARK).is_err());
    assert_eq!(
        std::fs::read_to_string(&elsewhere).unwrap(),
        "the owner's file\n",
        "a link at the mark's name never reaches the file behind it"
    );

    let planted = folder.path().join("planted");
    std::fs::create_dir(&planted).unwrap();
    let fifo = planted.join(graph::KEPT_OUT_MARK);
    let made = crate::spawn_gate::status(std::process::Command::new("/usr/bin/mkfifo").arg(&fifo))
        .unwrap();
    assert!(made.success());
    let (sent, wrote) = std::sync::mpsc::channel();
    let target = planted.clone();
    std::thread::spawn(move || {
        let _ = sent.send(graph::write_mark(&target, MARK).is_err());
    });
    let answer = wrote.recv_timeout(std::time::Duration::from_secs(5));
    let _reader = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(&fifo)
    };
    assert_eq!(
        answer,
        Ok(true),
        "a FIFO with no reader fails the write at once"
    );

    let reader = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(&fifo)
            .unwrap()
    };
    assert!(
        graph::write_mark(&planted, MARK).is_err(),
        "a FIFO that has a reader is still no mark file"
    );
    let mut taken = String::new();
    let _ = std::io::Read::read_to_string(&mut &reader, &mut taken);
    assert_eq!(taken, "", "nothing is written into the FIFO");
}

#[test]
#[should_panic(expected = "a keep-out mark fits what is read back")]
fn a_graph_mark_longer_than_what_is_read_back_is_never_written() {
    let folder = tempfile::tempdir().unwrap();
    let long = "m".repeat(graph::MARK_BYTES_MAX as usize + 1);
    let _ = graph::write_mark(folder.path(), &long);
}

#[test]
fn a_graph_step_whose_graphify_is_refused_fails_with_the_refused_line() {
    let refused = crate::tools::Missing {
        tool: "graphify".into(),
        reason: crate::tools::Reason::Refused,
        needs: None,
        found: None,
        looked: Vec::new(),
        refused: Some(Box::new(crate::tools::Refused {
            file: std::path::PathBuf::from("/h/tool/bin/python3"),
            why: crate::tools::Refusal::OthersCanChange(std::path::PathBuf::from(
                "/h/py/bin/python3.13",
            )),
        })),
    };
    assert!(refused.line().len() <= 256, "{}", refused.line());
    assert_ne!(refused.line(), graph::GRAPHIFY_MISSING);
    assert_eq!(service::graphify_failure(&refused), refused.line());
    let deep = std::path::PathBuf::from(format!("/h/{}/python3", "deep/".repeat(40)));
    let long = crate::tools::Missing {
        refused: Some(Box::new(crate::tools::Refused {
            file: deep.clone(),
            why: crate::tools::Refusal::OthersCanChange(deep),
        })),
        ..refused
    };
    assert!(long.line().len() > 256);
    assert_eq!(service::graphify_failure(&long), service::GRAPHIFY_REFUSED);
    assert!(service::GRAPHIFY_REFUSED.len() <= 256);
    let missing = crate::setup::Dependency::Graphify.not_found();
    assert_eq!(service::graphify_failure(&missing), graph::GRAPHIFY_MISSING);
}
