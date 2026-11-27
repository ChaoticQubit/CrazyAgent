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
    let made = kernel_processes::spawn_gate::status(
        std::process::Command::new("/usr/bin/mkfifo").arg(&fifo),
    )
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
    let refused = kernel_processes::finding::Missing {
        tool: "graphify".into(),
        reason: kernel_processes::finding::Reason::Refused,
        needs: None,
        found: None,
        looked: Vec::new(),
        refused: Some(Box::new(kernel_processes::finding::Refused {
            file: std::path::PathBuf::from("/h/tool/bin/python3"),
            why: kernel_processes::finding::Refusal::OthersCanChange(std::path::PathBuf::from(
                "/h/py/bin/python3.13",
            )),
        })),
    };
    assert!(refused.line().len() <= 256, "{}", refused.line());
    assert_ne!(refused.line(), graph::GRAPHIFY_MISSING);
    assert_eq!(service::graphify_failure(&refused), refused.line());
    let deep = std::path::PathBuf::from(format!("/h/{}/python3", "deep/".repeat(40)));
    let long = kernel_processes::finding::Missing {
        refused: Some(Box::new(kernel_processes::finding::Refused {
            file: deep.clone(),
            why: kernel_processes::finding::Refusal::OthersCanChange(deep),
        })),
        ..refused
    };
    assert!(long.line().len() > 256);
    assert_eq!(service::graphify_failure(&long), service::GRAPHIFY_REFUSED);
    assert!(service::GRAPHIFY_REFUSED.len() <= 256);
    let missing = crate::setup::Dependency::Graphify.not_found();
    assert_eq!(service::graphify_failure(&missing), graph::GRAPHIFY_MISSING);
}

/// macOS only: by DEC-094 the graph build runs under Seatbelt alone, and Linux refuses it
/// with the line that `a_graph_build_on_linux_ends_failed_and_writes_nothing` asserts.
#[cfg(target_os = "macos")]
#[test]
fn a_folder_hidden_after_the_graph_never_reaches_the_prompt_with_the_graph_off() {
    let (_dir, repo, hidden) = graphed_then_hidden();
    assert_hidden_out_of_the_prompt(&repo, &hidden);
}

/// A graph build over a repository that holds the engine's data folder, as after the owner
/// moves that folder into a project, is refused before its profile is written, so graphify
/// never reads the engine's database or keys.
#[cfg(target_os = "macos")]
#[test]
fn a_graph_update_over_a_repo_holding_the_engine_root_is_refused_before_install() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::Builder::new()
        .prefix("ca-graph-")
        .tempdir_in("/tmp")
        .unwrap();
    let repo = dir.path().canonicalize().unwrap();
    git_init(&repo);
    let engine = repo.join("data").join("engine");
    let state = engine.join("research");
    for folder in ["agents", "profiles"] {
        std::fs::create_dir_all(state.join(folder)).unwrap();
    }
    std::fs::write(engine.join("engine.db"), "engine secret\n").unwrap();
    let tools = tempfile::Builder::new()
        .prefix("ca-graphify-")
        .tempdir_in("/tmp")
        .unwrap();
    let graphify = tools.path().canonicalize().unwrap().join("graphify");
    std::fs::write(&graphify, "#!/bin/sh\nexit 0\n").unwrap();
    std::fs::set_permissions(&graphify, std::fs::Permissions::from_mode(0o755)).unwrap();
    let ending = graph::update(
        &graphify,
        &repo,
        &state,
        "onboard-g-engine",
        &[],
        std::time::Duration::from_secs(60),
        &|| false,
    );
    assert!(
        matches!(
            &ending,
            Err(crate::research::runner::RunError::Sandbox(
                kernel_sandbox::error::SandboxError::Invalid(reason)
            )) if reason == kernel_sandbox::paths::SECRET_ROOT_REFUSED
        ),
        "{ending:?}"
    );
    assert!(!state.join("profiles").join("onboard-g-engine.sb").exists());
}
