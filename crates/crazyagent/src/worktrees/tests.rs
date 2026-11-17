use super::*;

fn reserved_record() -> LeaseRecord {
    LeaseRecord {
        lease: WorktreeLease {
            project_id: ProjectId::new("project").unwrap(),
            task_id: TaskId::new("task").unwrap(),
            repository: "/repository".into(),
            path: "/runtime/worktrees/task".into(),
            branch: "crazyagent/task".into(),
            owner_session: SessionId::new("owner").unwrap(),
            native_descendant: None,
            state: LeaseState::Reserved,
        },
        admission: Admission {
            repository: "/repository".into(),
            root: Identity {
                device: 1,
                inode: 2,
            },
            metadata: Identity {
                device: 1,
                inode: 3,
            },
        },
        head: "a".repeat(40),
        directory: None,
        metadata: None,
    }
}

#[test]
#[should_panic(expected = "lease repository must match its admission")]
fn internal_record_assertion_detects_cross_repository_lease() {
    let mut record = reserved_record();
    record.lease.repository = "/other-repository".into();
    assert_record_consistent(&record);
}

#[test]
#[should_panic(expected = "ready lease requires directory identity")]
fn internal_record_assertion_detects_unsealed_ready_lease() {
    let mut record = reserved_record();
    record.lease.state = LeaseState::Ready;
    assert_record_consistent(&record);
}

#[test]
fn repository_config_rule_refuses_keys_git_runs_and_accepts_tool_metadata() {
    for (key, value) in [
        ("branch.feature/x.vscode-merge-base", "origin/trunk"),
        ("branch.trunk.remote", "origin"),
        ("branch.trunk.merge", "refs/heads/trunk"),
        ("branch.trunk.description", "Owner notes"),
        (
            "remote.origin.url",
            "https://example.invalid/owner/project.git",
        ),
        (
            "remote.origin.pushurl",
            "https://example.invalid/owner/project.git",
        ),
        ("remote.origin.fetch", "+refs/heads/*:refs/remotes/origin/*"),
        ("remote.origin.gh-resolved", "base"),
        ("core.repositoryformatversion", "0"),
        ("core.repositoryformatversion", "1"),
        ("core.bare", "false"),
        ("core.filemode", "true"),
        ("core.logallrefupdates", "true"),
        ("core.ignorecase", "true"),
        ("core.precomposeunicode", "true"),
        ("core.autocrlf", "input"),
        ("extensions.worktreeconfig", "true"),
        ("extensions.worktreeconfig", "FALSE"),
        ("codex.localenvironmentconfigpath", "__none__"),
        ("user.name", "Owner"),
        ("user.email", "owner@example.invalid"),
        ("init.defaultbranch", "trunk"),
        ("alias.st", "status"),
        ("lfs.repositoryformatversion", "0"),
        ("submodule.lib.url", "https://example.invalid/lib.git"),
        ("pull.rebase", "true"),
    ] {
        assert!(
            refused_key(key, value).is_none(),
            "{key}={value} must stay accepted"
        );
    }
    let mut seen = Vec::new();
    for (key, value, code) in [
        ("core.repositoryformatversion", "2", "repository_format"),
        ("core.bare", "true", "core_bare"),
        (
            "extensions.worktreeconfig",
            "maybe",
            "extensions_worktreeconfig",
        ),
        ("extensions.objectformat", "sha256", "repository_extension"),
        ("core.hookspath", ".githooks", "core_hookspath"),
        ("hook.lint.command", "make lint", "hook_command"),
        ("hook.lint.event", "pre-commit", "hook_command"),
        ("core.fsmonitor", "true", "core_fsmonitor"),
        ("core.sshcommand", "ssh -F config", "core_sshcommand"),
        ("core.gitproxy", "proxy-command", "core_gitproxy"),
        ("core.editor", "vim", "core_editor"),
        ("core.pager", "less", "core_pager"),
        ("core.askpass", "prompt", "core_askpass"),
        (
            "core.alternaterefscommand",
            "list",
            "core_alternaterefscommand",
        ),
        ("core.worktree", "/elsewhere", "core_worktree"),
        ("core.attributesfile", "/elsewhere", "core_attributesfile"),
        (
            "filter.lfs.process",
            "git-lfs filter-process",
            "filter_driver",
        ),
        ("filter.lfs.required", "true", "filter_driver"),
        ("filter.a.b.smudge", "command", "filter_driver"),
        ("diff.pdf.textconv", "pdftotext", "diff_driver"),
        ("diff.pdf.command", "command", "diff_driver"),
        ("diff.external", "command", "diff_external"),
        ("difftool.vim.cmd", "command", "difftool"),
        ("difftool.prompt", "false", "difftool"),
        ("merge.ours.driver", "true", "merge_driver"),
        ("mergetool.vim.cmd", "command", "mergetool"),
        ("sequence.editor", "command", "sequence_editor"),
        ("credential.helper", "store", "credential"),
        (
            "credential.https://example.invalid.helper",
            "store",
            "credential",
        ),
        ("gpg.program", "command", "gpg_program"),
        ("gpg.openpgp.program", "command", "gpg_program"),
        ("gpg.ssh.defaultkeycommand", "command", "gpg_program"),
        ("pager.log", "command", "pager_command"),
        ("trailer.sign.command", "command", "trailer_command"),
        ("trailer.sign.cmd", "command", "trailer_command"),
        (
            "interactive.difffilter",
            "command",
            "interactive_difffilter",
        ),
        ("submodule.lib.update", "!command", "submodule_update"),
        (
            "url.https://example.invalid/.insteadof",
            "git@example.invalid:",
            "url_rewrite",
        ),
        ("url.ssh://h/.pushinsteadof", "https://h/", "url_rewrite"),
        ("include.path", "/elsewhere", "include"),
        ("includeif.gitdir:/a.b/.path", "/elsewhere", "include_if"),
        ("remote.origin.uploadpack", "command", "remote_command"),
        ("remote.origin.receivepack", "command", "remote_command"),
        ("remote.origin.vcs", "helper", "remote_command"),
        ("remote.origin.proxy", "http://proxy", "remote_command"),
        ("http.proxy", "http://proxy", "http_proxy"),
        (
            "http.https://example.invalid.proxy",
            "http://proxy",
            "http_proxy",
        ),
        ("protocol.allow", "always", "protocol_allow"),
        ("protocol.ext.allow", "always", "protocol_allow"),
        ("uploadpack.packobjectshook", "command", "uploadpack_hook"),
        ("gc.recentobjectshook", "command", "gc_hook"),
        ("sendemail.sendmailcmd", "command", "sendemail_command"),
        (
            "sendemail.smtpserver",
            "/usr/bin/command",
            "sendemail_command",
        ),
        ("imap.tunnel", "command", "imap_tunnel"),
        ("browser.firefox.cmd", "command", "browser_command"),
        ("man.viewer.path", "command", "man_command"),
        ("guitool.lint.cmd", "command", "guitool_command"),
    ] {
        assert_eq!(
            refused_key(key, value).map(|rule| rule.code),
            Some(code),
            "{key}={value}"
        );
        seen.push(code);
    }
    let mut codes: Vec<_> = REFUSED_KEYS.iter().map(|rule| rule.code).collect();
    codes.sort_unstable();
    codes.dedup();
    assert_eq!(codes.len(), REFUSED_KEYS.len(), "rule codes must be unique");
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, codes, "every rule needs a refused case");
    assert_eq!(
        refused_config_line("repository_config_refused:core_hookspath", "/owner/repo").as_deref(),
        Some(concat!(
            "/owner/repo has the Git setting core.hooksPath, which runs hooks from a ",
            "folder the repository chooses. Remove it from the repository's .git/config, ",
            "then send again."
        ))
    );
    assert_eq!(refused_config_line("unsupported", "/owner/repo"), None);
    assert_eq!(
        refused_config_line("repository_config_refused:unknown", "/owner/repo"),
        None
    );
}

#[test]
fn descriptor_materialization_resists_renamed_root_symlink_swap() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("worktree");
    let retained = temporary.path().join("retained");
    let outside = temporary.path().join("outside");
    std::fs::create_dir(&root).unwrap();
    std::fs::create_dir(&outside).unwrap();
    let directory = std::fs::File::open(&root).unwrap();
    std::fs::rename(&root, &retained).unwrap();
    std::os::unix::fs::symlink(&outside, &root).unwrap();
    Folders::new(&directory)
        .write_file(
            Path::new("nested/tracked"),
            &mut &b"source bytes"[..],
            12,
            false,
        )
        .unwrap();
    assert_eq!(
        std::fs::read(retained.join("nested/tracked")).unwrap(),
        b"source bytes"
    );
    assert!(!outside.join("nested").exists());
    std::os::unix::fs::symlink(&outside, retained.join("escape")).unwrap();
    assert!(
        Folders::new(&directory)
            .write_file(Path::new("escape/file"), &mut &b"forbidden"[..], 9, false)
            .is_err()
    );
    assert!(!outside.join("file").exists());
    let mut folders = Folders::new(&directory);
    folders
        .write_file(Path::new("open/first"), &mut &b"one"[..], 3, false)
        .unwrap();
    std::fs::rename(retained.join("open"), retained.join("moved")).unwrap();
    std::os::unix::fs::symlink(&outside, retained.join("open")).unwrap();
    folders
        .write_file(Path::new("open/second"), &mut &b"two"[..], 3, false)
        .unwrap();
    assert_eq!(
        std::fs::read(retained.join("moved/second")).unwrap(),
        b"two"
    );
    assert!(!outside.join("second").exists());
}

fn committed_repository(root: &Path) -> PathBuf {
    let repository = root.join("repository");
    std::fs::create_dir(&repository).unwrap();
    std::fs::create_dir_all(repository.join("one/two")).unwrap();
    std::fs::write(repository.join("top"), b"12345").unwrap();
    std::fs::write(repository.join("one/middle"), b"123").unwrap();
    std::fs::write(repository.join("one/two/leaf"), b"1").unwrap();
    std::os::unix::fs::symlink("../top", repository.join("one/link")).unwrap();
    for args in [
        &["init", "-q", "-b", "main"][..],
        &["add", "-A"],
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@invalid",
            "commit",
            "-q",
            "-m",
            "base",
        ],
    ] {
        git(&repository, args).unwrap();
    }
    repository
}

#[test]
#[cfg(unix)]
fn submodule_stores_are_created_private_and_idempotent() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repository");
    let admin = temporary.path().join("admin");
    std::fs::create_dir_all(repository.join(".git")).unwrap();
    std::fs::create_dir(&admin).unwrap();
    ensure_submodule_stores(&repository, &admin).unwrap();
    for store in [repository.join(".git/modules"), admin.join("modules")] {
        let metadata = std::fs::metadata(&store).unwrap();
        assert!(metadata.is_dir(), "{} is not a folder", store.display());
        assert_eq!(metadata.permissions().mode() & 0o777, 0o700);
    }
    ensure_submodule_stores(&repository, &admin).unwrap();
}

#[test]
#[cfg(unix)]
fn a_submodule_store_that_is_a_file_or_a_link_is_refused_not_trusted() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = temporary.path().join("repository");
    let admin = temporary.path().join("admin");
    let elsewhere = temporary.path().join("elsewhere");
    std::fs::create_dir_all(repository.join(".git")).unwrap();
    std::fs::create_dir(&admin).unwrap();
    std::fs::create_dir(&elsewhere).unwrap();
    let common = repository.join(".git").join("modules");
    std::fs::write(&common, b"planted").unwrap();
    assert_eq!(
        ensure_submodule_stores(&repository, &admin).unwrap_err().0,
        "submodule_store_invalid"
    );
    assert_eq!(std::fs::read(&common).unwrap(), b"planted");
    std::fs::remove_file(&common).unwrap();
    let private = admin.join("modules");
    std::os::unix::fs::symlink(&elsewhere, &private).unwrap();
    assert_eq!(
        ensure_submodule_stores(&repository, &admin).unwrap_err().0,
        "submodule_store_invalid"
    );
    assert!(
        std::fs::symlink_metadata(&private)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

/// A repository whose .git holds what Git makes and removes while it works: lock files,
/// a temporary pack, loose objects in fan-out folders, another worktree's admin folder
/// and a folder of a running rebase. Input: the folder to make it in. Output: the
/// repository.
fn metadata_folder(root: &Path) -> PathBuf {
    let repository = root.join("repository");
    let git = repository.join(".git");
    for folder in [
        "refs/heads",
        "objects/ab",
        "objects/cd",
        "objects/ef",
        "objects/pack",
        "worktrees/other",
        "rebase-merge",
    ] {
        std::fs::create_dir_all(git.join(folder)).unwrap();
    }
    for file in [
        "HEAD",
        "index.lock",
        "refs/heads/main.lock",
        "objects/ab/0123",
        "objects/ab/4567",
        "objects/pack/tmp_pack_1",
    ] {
        std::fs::write(git.join(file), b"kept\n").unwrap();
    }
    repository
}

/// Removes one entry as Git does when it is done with it. Input: the file or folder.
fn unlink(path: &Path) {
    if std::fs::symlink_metadata(path).unwrap().is_dir() {
        std::fs::remove_dir_all(path).unwrap();
    } else {
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn a_git_entry_removed_between_its_listing_and_its_read_is_no_finding() {
    let root = tempfile::tempdir().unwrap();
    let repository = metadata_folder(root.path());
    let git = repository.join(".git");
    let gone = [
        "index.lock",
        "refs/heads/main.lock",
        "objects/pack/tmp_pack_1",
        "objects/ab/0123",
        "objects/cd",
        "worktrees/other",
    ]
    .map(|name| git.join(name));
    let emptied = ["rebase-merge", "objects/ef"].map(|name| git.join(name));
    let step = {
        let (gone, emptied) = (gone.clone(), emptied.clone());
        BeforeRead::set(move |path, listing| {
            let named = if listing { &emptied[..] } else { &gone[..] };
            if named.iter().any(|entry| entry == path) {
                unlink(path);
            }
        })
    };
    validate_metadata(&repository).unwrap();
    drop(step);
    for entry in gone.iter().chain(&emptied) {
        assert!(
            std::fs::symlink_metadata(entry).is_err(),
            "the walk never read {}",
            entry.display()
        );
    }
    assert!(git.join("objects/ab/4567").is_file());
    validate_metadata(&repository).unwrap();
}

#[test]
fn a_link_or_a_pipe_put_in_a_listed_entry_s_place_is_still_refused() {
    for (name, pipe) in [
        ("index.lock", false),
        ("index.lock", true),
        ("objects/ab/0123", false),
        ("objects/ab/0123", true),
        ("refs/heads", false),
        ("refs/heads", true),
        ("objects/cd", false),
        ("worktrees/other", false),
    ] {
        let root = tempfile::tempdir().unwrap();
        let repository = metadata_folder(root.path());
        let outside = root.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        let swapped = repository.join(".git").join(name);
        let step = {
            let swapped = swapped.clone();
            BeforeRead::set(move |path, listing| {
                let kept = std::fs::symlink_metadata(path)
                    .is_ok_and(|entry| entry.is_file() || entry.is_dir());
                if path != swapped || listing || !kept {
                    return;
                }
                unlink(path);
                if pipe {
                    let made = crate::spawn_gate::status(
                        std::process::Command::new("/usr/bin/mkfifo").arg(path),
                    )
                    .unwrap();
                    assert!(made.success());
                } else {
                    std::os::unix::fs::symlink(&outside, path).unwrap();
                }
            })
        };
        let refused = validate_metadata(&repository).unwrap_err();
        drop(step);
        assert_eq!(
            refused.0,
            format!("{UNTRUSTED_GIT_ENTRY}{name}"),
            "pipe {pipe}"
        );
        let left = std::fs::symlink_metadata(&swapped).unwrap();
        assert!(
            !left.is_file() && !left.is_dir(),
            "{name} was never swapped"
        );
    }
}

#[test]
fn a_missing_start_or_an_unreadable_folder_is_a_failure_not_an_entry_gone() {
    let root = tempfile::tempdir().unwrap();
    let repository = metadata_folder(root.path());
    let git = repository.join(".git");
    validate_admin(&repository, &git.join("worktrees/other")).unwrap();
    assert!(validate_admin(&repository, &git.join("worktrees/absent")).is_err());
    let closed = git.join("refs/heads");
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o000)).unwrap();
    let refused = validate_metadata(&repository);
    std::fs::set_permissions(&closed, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(refused.is_err(), "an unreadable folder was skipped as gone");
    validate_metadata(&repository).unwrap();
    std::fs::remove_dir_all(&git).unwrap();
    assert!(validate_metadata(&repository).is_err());
}

#[test]
fn only_a_read_failure_of_a_lease_check_is_tried_once_more() {
    let run = |answers: &[Result<(), &str>]| {
        let calls = std::cell::Cell::new(0);
        let result = checked(|| {
            let answer = answers[calls.get()];
            calls.set(calls.get() + 1);
            answer.map_err(error)
        });
        (
            result.map_err(|(problem, retried)| (problem.0, retried)),
            calls.get(),
        )
    };
    let missing = "No such file or directory (os error 2)";
    assert_eq!(run(&[Ok(())]), (Ok(()), 1));
    assert_eq!(run(&[Err(missing), Ok(())]), (Ok(()), 2));
    assert_eq!(
        run(&[Err(missing), Err("Permission denied (os error 13)")]),
        (Err(("Permission denied (os error 13)".to_owned(), true)), 2)
    );
    assert_eq!(
        run(&[Err(missing), Err("untrusted_git_entry:index.lock")]),
        (Err(("untrusted_git_entry:index.lock".to_owned(), true)), 2)
    );
    for fast in GIT_FAST_FAILURES {
        assert_eq!(run(&[Err(fast), Ok(())]), (Ok(()), 2), "{fast}");
        assert_eq!(
            run(&[Err(fast), Err(fast)]),
            (Err((fast.to_owned(), true)), 2),
            "{fast}"
        );
    }
    for rule in [
        "git_deadline",
        "untrusted_git_entry:index.lock",
        "git_metadata_capacity",
        "unsupported_shared_or_partial_git_metadata",
        "repository_path_not_canonical",
        "untrusted_filesystem_identity",
    ] {
        assert_eq!(
            run(&[Err(rule), Ok(())]),
            (Err((rule.to_owned(), false)), 1),
            "{rule}"
        );
    }
}

#[test]
fn a_lease_log_line_names_a_code_and_never_text_or_a_path_outside_git() {
    for (failure, logged) in [
        (
            "No such file or directory (os error 2)",
            "lease_read_failed_os_2",
        ),
        ("Permission denied (os error 13)", "lease_read_failed_os_13"),
        (
            "/Users/owner/project: Permission denied (os error 13)",
            "lease_read_failed_os_13",
        ),
        (
            "missing field `lease` at line 1 column 2",
            "lease_read_failed",
        ),
        ("failed (os error 1234567)", "lease_read_failed"),
        ("", "lease_read_failed"),
        ("git_deadline", "git_deadline"),
        (
            "repository_path_not_canonical",
            "repository_path_not_canonical",
        ),
        (
            "untrusted_git_entry:refs/heads/main.lock",
            "untrusted_git_entry:refs/heads/main.lock",
        ),
        ("untrusted_git_entry:/Users/owner/x", "untrusted_git_entry"),
        ("untrusted_git_entry:../../outside", "untrusted_git_entry"),
        ("untrusted_git_entry:a b", "untrusted_git_entry"),
        ("some_rule:/Users/owner/project", "some_rule"),
    ] {
        assert_eq!(logged_failure(&error(failure)), logged, "{failure}");
    }
}

#[test]
fn a_failing_lease_is_logged_once_per_cause_and_the_log_is_bounded() {
    let new = || LeaseLog {
        leases: std::collections::BTreeMap::new(),
        full: false,
    };
    let mut log = new();
    let lost = "worktree_lease_uncertain cause=lease_read_failed_os_2 retried=true";
    let linked = "worktree_lease_uncertain cause=untrusted_git_entry:index retried=false";
    let mut written = Vec::new();
    for _ in 0..50 {
        written.extend(log.line("task-a", "/repo", "lease_read_failed_os_2", true));
        written.extend(log.line("task-b", "/repo", "untrusted_git_entry:index", false));
        written.extend(log.line("task-a", "/other", "untrusted_git_entry:index", false));
    }
    assert_eq!(written, [lost, linked, linked], "one line a lease");
    assert!(
        written
            .iter()
            .all(|line| !line.contains("task") && !line.contains("repo"))
    );

    let mut log = new();
    let mut flapping = 0;
    for turn in 0..50 {
        let cause = if turn % 2 == 0 {
            "git_deadline"
        } else {
            "git_reader_failed"
        };
        flapping += usize::from(log.line("task-a", "/repo", cause, false).is_some());
    }
    assert_eq!(flapping, LOGGED_LINES_PER_LEASE_MAX as usize);

    let mut log = new();
    for lease in 0..LOGGED_LEASES_MAX {
        assert!(
            log.line(&format!("task-{lease}"), "/repo", "git_deadline", false)
                .is_some()
        );
    }
    assert_eq!(
        log.line("task-late", "/repo", "git_deadline", false)
            .as_deref(),
        Some("worktree_lease_log_full")
    );
    assert_eq!(log.line("task-late", "/repo", "git_deadline", false), None);
    assert_eq!(log.line("task-later", "/repo", "git_deadline", false), None);
    assert_eq!(log.line("task-0", "/repo", "git_deadline", false), None);
    assert!(
        log.line("task-0", "/repo", "git_reader_failed", false)
            .is_some()
    );
    assert_eq!(log.leases.len(), LOGGED_LEASES_MAX);
}

#[test]
fn tree_listing_refuses_a_tree_past_its_file_byte_or_depth_bound() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = committed_repository(temporary.path());
    let later = Instant::now() + Duration::from_secs(5);
    let bounds = |files, bytes, components| TreeBounds {
        files,
        bytes,
        components,
    };
    let entries = tree_entries(&repository, "HEAD", &bounds(4, 15, 3), later).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|e| (e.path.to_str().unwrap(), e.size, e.link))
            .collect::<Vec<_>>(),
        [
            ("one/link", 6, true),
            ("one/middle", 3, false),
            ("one/two/leaf", 1, false),
            ("top", 5, false),
        ]
    );
    for (bounds, code) in [
        (bounds(3, 15, 3), "tree_file_capacity"),
        (bounds(4, 14, 3), "tree_byte_capacity"),
        (bounds(4, 15, 2), "tree_path_depth"),
    ] {
        assert_eq!(
            tree_entries(&repository, "HEAD", &bounds, later)
                .unwrap_err()
                .0,
            code
        );
    }
}

/// A tree path with a part a case-insensitive HFS+ volume opens as `.git`, such as
/// `.g\u{200c}it` or `\u{feff}.GIT`, is refused like `.git` itself, so a lease never
/// writes into a git folder on such a volume (RV-DS241-148).
#[test]
fn tree_listing_refuses_a_path_part_hfs_opens_as_git() {
    for part in [".g\u{200c}it", "\u{feff}.GIT"] {
        let temporary = tempfile::tempdir().unwrap();
        let repository = committed_repository(temporary.path());
        let folder = repository.join("sub").join(part);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("config"), b"[core]\n").unwrap();
        for args in [
            &[
                "-c",
                "core.protectHFS=false",
                "-c",
                "core.protectNTFS=false",
                "add",
                "-A",
            ][..],
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@invalid",
                "commit",
                "-q",
                "-m",
                "planted",
            ],
        ] {
            git(&repository, args).unwrap();
        }
        let later = Instant::now() + Duration::from_secs(5);
        assert_eq!(
            tree_entries(&repository, "HEAD", &TREE_BOUNDS, later)
                .unwrap_err()
                .0,
            "unsupported_or_unsafe_tree_entry",
            "{part:?}"
        );
    }
}

#[test]
fn writing_a_tree_stops_at_its_deadline() {
    let temporary = tempfile::tempdir().unwrap();
    let repository = committed_repository(temporary.path());
    let later = Instant::now() + Duration::from_secs(5);
    let entries = tree_entries(&repository, "HEAD", &TREE_BOUNDS, later).unwrap();
    let target = temporary.path().join("copy");
    std::fs::create_dir(&target).unwrap();
    let directory = std::fs::File::open(&target).unwrap();
    assert_eq!(
        write_tree(&repository, &directory, &entries, Instant::now())
            .unwrap_err()
            .0,
        "materialization_deadline"
    );
    let copy = temporary.path().join("second-copy");
    std::fs::create_dir(&copy).unwrap();
    let directory = std::fs::File::open(&copy).unwrap();
    assert!(
        write_tree(&repository, &directory, &entries, later)
            .unwrap()
            .is_empty()
    );
    assert_eq!(std::fs::read(copy.join("one/two/leaf")).unwrap(), b"1");
    assert_eq!(
        std::fs::read_link(copy.join("one/link")).unwrap(),
        Path::new("../top")
    );
}

#[test]
fn a_tracked_symlink_stays_inside_only_by_leading_parent_steps_within_its_depth() {
    for (link, target, inside) in [
        ("CLAUDE.md", "AGENTS.md", true),
        ("docs/current", "../src/m0", true),
        ("a/b/up", "../..", true),
        ("a/b/c", "../../x/y", true),
        ("top", "../outside", false),
        ("a/b/c", "../../../outside", false),
        ("abs", "/etc/passwd", false),
        ("a/mid", "x/../../outside", false),
        ("a/dot", "./x", false),
        ("a/empty", "x//y", false),
        ("a/trailing", "x/", false),
        ("none", "", false),
        ("a/git", "../.git/config", false),
        ("a/git-case", "../.GIT", false),
        ("a/git-hfs", "../.g\u{200c}it/config", false),
        ("a/git-bom", "../\u{feff}.GIT", false),
        ("a/nul", "x\0y", false),
    ] {
        assert_eq!(
            link_stays_inside(Path::new(link), target.as_bytes()),
            inside,
            "{link} -> {target}"
        );
    }
    assert!(!link_stays_inside(Path::new("bytes"), &[0xff, 0xfe]));
}

#[cfg(unix)]
#[test]
fn only_a_missing_config_or_the_exact_remote_lock_counts_as_kept() {
    let temporary = tempfile::tempdir().unwrap();
    let metadata = temporary.path();
    let config = metadata.join("config.worktree");
    assert_eq!(remote_lock(metadata), None);
    assert!(remote_lock_kept(metadata));
    std::fs::write(&config, REMOTE_LOCK).unwrap();
    assert_eq!(remote_lock(metadata), Some(true));
    assert!(remote_lock_kept(metadata));
    std::fs::write(&config, PREVIOUS_REMOTE_LOCK).unwrap();
    assert_eq!(remote_lock(metadata), Some(false));
    assert!(remote_lock_kept(metadata));
    let mut previous_longer = PREVIOUS_REMOTE_LOCK.to_vec();
    previous_longer.extend_from_slice(b"[core]\n\tfsmonitor = touch /tmp/x\n");
    std::fs::write(&config, &previous_longer).unwrap();
    assert!(!remote_lock_kept(metadata));
    let mut longer = REMOTE_LOCK.to_vec();
    longer.extend_from_slice(b"[core]\n\tfsmonitor = touch /tmp/x\n");
    std::fs::write(&config, &longer).unwrap();
    assert!(!remote_lock_kept(metadata));
    let mut same_length = REMOTE_LOCK.to_vec();
    same_length[2] = b'x';
    std::fs::write(&config, &same_length).unwrap();
    assert!(!remote_lock_kept(metadata));
    std::fs::remove_file(&config).unwrap();
    let elsewhere = metadata.join("elsewhere");
    std::fs::write(&elsewhere, REMOTE_LOCK).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &config).unwrap();
    assert!(!remote_lock_kept(metadata));
    std::fs::remove_file(&config).unwrap();
    std::fs::create_dir(&config).unwrap();
    assert!(!remote_lock_kept(metadata));
}

#[cfg(unix)]
#[test]
fn a_missing_cut_short_or_previous_lock_is_replaced_and_any_other_config_is_left_alone() {
    let temporary = tempfile::tempdir().unwrap();
    let metadata = temporary.path();
    let config = metadata.join("config.worktree");
    let partial = metadata.join(LOCK_PARTIAL);
    let repaired: [&[u8]; 6] = [
        b"",
        &REMOTE_LOCK[..1],
        &REMOTE_LOCK[..REMOTE_LOCK.len() - 1],
        PREVIOUS_REMOTE_LOCK,
        &PREVIOUS_REMOTE_LOCK[..PREVIOUS_REMOTE_LOCK.len() - 3],
        REMOTE_LOCK,
    ];
    assert_eq!(lock_state(metadata), LockState::Open);
    lock_metadata(metadata).unwrap();
    assert_eq!(std::fs::read(&config).unwrap(), REMOTE_LOCK);
    for held in repaired {
        std::fs::write(&config, held).unwrap();
        std::fs::write(&partial, b"[core]\n\tfsmonitor = touch /tmp/x\n").unwrap();
        let expected = if held == REMOTE_LOCK {
            LockState::Locked
        } else {
            LockState::Open
        };
        assert_eq!(lock_state(metadata), expected, "{held:?}");
        lock_metadata(metadata).unwrap();
        assert_eq!(std::fs::read(&config).unwrap(), REMOTE_LOCK, "{held:?}");
        assert_eq!(remote_lock(metadata), Some(true));
        if held != REMOTE_LOCK {
            assert!(!partial.exists(), "{held:?}");
        }
    }
    let mut longer = REMOTE_LOCK.to_vec();
    longer.extend_from_slice(b"\n");
    let mut changed = REMOTE_LOCK[..REMOTE_LOCK.len() / 2].to_vec();
    changed[1] = b'x';
    let foreign: [&[u8]; 3] = [b"[core]\n\tfsmonitor = touch /tmp/x\n", &longer, &changed];
    for held in foreign {
        std::fs::write(&config, held).unwrap();
        assert_eq!(lock_state(metadata), LockState::Foreign, "{held:?}");
        assert_eq!(
            lock_metadata(metadata).unwrap_err().0,
            "worktree_config_present"
        );
        assert_eq!(std::fs::read(&config).unwrap(), held);
    }
    std::fs::remove_file(&config).unwrap();
    let elsewhere = metadata.join("elsewhere");
    std::fs::write(&elsewhere, &REMOTE_LOCK[..4]).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &config).unwrap();
    assert_eq!(lock_state(metadata), LockState::Foreign);
    assert!(lock_metadata(metadata).is_err());
    assert_eq!(std::fs::read(&elsewhere).unwrap(), &REMOTE_LOCK[..4]);
    assert!(
        std::fs::symlink_metadata(&config)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(unix)]
#[test]
fn the_worktree_config_switch_is_read_like_git_reads_it() {
    let cases: [(&[u8], bool); 10] = [
        (b"", false),
        (b"core.bare\nfalse\0", false),
        (b"extensions.worktreeconfig\ntrue\0", true),
        (b"extensions.worktreeConfig\nYes\0", true),
        (b"extensions.worktreeconfig\n1\0", true),
        (b"extensions.worktreeconfig\0", true),
        (b"extensions.worktreeconfig\nfalse\0", false),
        (
            b"extensions.worktreeconfig\ntrue\0extensions.worktreeconfig\noff\0",
            false,
        ),
        (
            b"extensions.worktreeconfig\nno\0extensions.worktreeconfig\non\0",
            true,
        ),
        (b"remote.origin.url\nextensions.worktreeconfig\0", false),
    ];
    for (listing, on) in cases {
        assert_eq!(
            worktree_config_on(listing),
            on,
            "{}",
            String::from_utf8_lossy(listing)
        );
    }
}

const WORKTREE_CONFIG_RACERS: usize = 8;
const WORKTREE_CONFIG_ROUNDS: usize = 10;

/// First-time setups of one repository that turn `extensions.worktreeConfig` on at the
/// same moment all succeed: one that loses `.git/config.lock` to another reads the switch
/// again and finds it on, so no lease or worktree fails on the race.
#[cfg(unix)]
#[test]
fn first_time_worktree_config_writes_that_race_all_succeed() {
    for _ in 0..WORKTREE_CONFIG_ROUNDS {
        let temporary = tempfile::tempdir().unwrap();
        let repository = committed_repository(temporary.path());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(WORKTREE_CONFIG_RACERS));
        let racers: Vec<_> = (0..WORKTREE_CONFIG_RACERS)
            .map(|_| {
                let (repository, barrier) = (repository.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    turn_on_worktree_config(&repository)
                })
            })
            .collect();
        for racer in racers {
            assert_eq!(racer.join().unwrap(), Ok(()));
        }
        let listing = git(
            &repository,
            &["config", "--local", "--no-includes", "--null", "--list"],
        )
        .unwrap();
        assert!(worktree_config_on(&listing));
    }
}

/// Appends tracked branch sections to a repository's config, as a repository with many
/// branches holds them. Inputs: the repository and how many branches. Output: none; the
/// config file grows by about 105 bytes a branch.
#[cfg(unix)]
fn track_branches(repository: &Path, count: usize) {
    use std::fmt::Write;
    let config = repository.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    for branch in 0..count {
        let name = format!("feature/scholar-scope-{branch:04}");
        write!(
            text,
            "[branch \"{name}\"]\n\tremote = origin\n\tmerge = refs/heads/{name}\n"
        )
        .unwrap();
    }
    std::fs::write(&config, text).unwrap();
}

#[cfg(unix)]
#[test]
fn a_repository_whose_config_lists_more_than_64_kib_is_inspected() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repository = committed_repository(&root);
    track_branches(&repository, 1000);
    let listing = git(
        &repository,
        &["config", "--local", "--no-includes", "--null", "--list"],
    )
    .unwrap();
    assert!(listing.len() > 100_000);
    let admission = inspect(repository.to_str().unwrap()).unwrap();
    assert_eq!(Path::new(&admission.repository), repository);
}

#[cfg(unix)]
#[test]
fn a_refused_key_after_64_kib_of_config_is_still_refused() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repository = committed_repository(&root);
    track_branches(&repository, 1000);
    let config = repository.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[core]\n\tfsmonitor = /bin/true\n");
    std::fs::write(&config, text).unwrap();
    assert_eq!(
        inspect(repository.to_str().unwrap()).unwrap_err().0,
        "repository_config_refused:core_fsmonitor"
    );
}

#[cfg(unix)]
#[test]
fn a_config_listing_past_the_git_output_bound_is_refused() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repository = committed_repository(&root);
    let config = repository.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    text.push_str("[user]\n\tname = ");
    text.push_str(&"n".repeat(OUTPUT_BYTES_MAX + 1));
    text.push('\n');
    std::fs::write(&config, text).unwrap();
    assert_eq!(
        inspect(repository.to_str().unwrap()).unwrap_err().0,
        "git_operation_failed_or_output_capacity"
    );
}

/// Fills a folder inside a repository's .git with empty files, as a long-used
/// repository's `rr-cache` or `logs` holds them. Inputs: the repository, the folder's
/// name and how many files. Output: none.
#[cfg(unix)]
fn fill_git_folder(repository: &Path, name: &str, count: usize) {
    let folder = repository.join(".git").join(name);
    std::fs::create_dir(&folder).unwrap();
    for entry in 0..count {
        std::fs::write(folder.join(format!("{entry:06}")), b"").unwrap();
    }
}

#[cfg(unix)]
#[test]
fn a_repository_whose_git_folder_holds_more_than_8192_entries_is_inspected() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repository = committed_repository(&root);
    fill_git_folder(&repository, "rr-cache", 9000);
    let admission = inspect(repository.to_str().unwrap()).unwrap();
    assert_eq!(Path::new(&admission.repository), repository);
}

#[cfg(unix)]
#[test]
fn a_git_folder_past_the_metadata_bound_is_refused() {
    let temporary = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(temporary.path()).unwrap();
    let repository = committed_repository(&root);
    fill_git_folder(&repository, "rr-cache", METADATA_ENTRIES_MAX + 1);
    assert_eq!(
        inspect(repository.to_str().unwrap()).unwrap_err().0,
        "git_metadata_capacity"
    );
}

/// The engine's worktree git, asked outright for the signature of an owner commit in any
/// armor, runs none of the repository's signature programs (RV-DS241-39).
#[test]
fn engine_worktree_git_runs_no_repository_signature_program_in_any_armor() {
    use crate::landing::signature_tests::{Armor, signed};
    for armor in Armor::ALL {
        let signed = signed(armor);
        git(
            &signed.root,
            &["log", "--show-signature", "-1", "--format=%s"],
        )
        .unwrap();
        assert_eq!(signed.ran(), Vec::<String>::new(), "{armor:?}");
    }
}

/// Runs [`restarted_at`] for a lease of `/repository`, a folder that does not exist, so
/// its volume never reads as renumbered. Inputs: the state and the answers. Output: the
/// state stored after the start and how many checks ran.
fn restarted(state: LeaseState, answers: &[Result<(), &str>]) -> (LeaseState, usize) {
    restarted_at("/repository", state, answers)
}

/// Stores one lease of `repository` at `state` with both worktree identities pinned in a
/// fresh lease table, then runs one engine start's reconcile with a lease check that
/// gives `answers` in turn, the last one again once they run out. Inputs: the
/// repository, the state and the answers. Output: the state stored and the check count.
fn restarted_at(
    repository: &str,
    state: LeaseState,
    answers: &[Result<(), &str>],
) -> (LeaseState, usize) {
    assert!(!answers.is_empty());
    let connection = Connection::open_in_memory().unwrap();
    initialize(&connection).unwrap();
    let mut record = reserved_record();
    record.lease.repository = repository.into();
    record.admission.repository = repository.into();
    record.lease.state = state;
    record.directory = Some(Identity {
        device: 1,
        inode: 4,
    });
    record.metadata = Some(Identity {
        device: 1,
        inode: 5,
    });
    let inserted = connection
        .execute(
            "INSERT INTO worktree_leases VALUES (?1, ?2, ?3, ?4)",
            params![
                record.lease.task_id.as_str(),
                record.lease.repository,
                record.lease.path,
                serde_json::to_string(&record).unwrap()
            ],
        )
        .unwrap();
    assert_eq!(inserted, 1);
    let mut calls = 0;
    reconcile_with(&connection, |checked| {
        assert_eq!(checked.lease, record.lease);
        let answer = answers[calls.min(answers.len() - 1)];
        calls += 1;
        answer.map_err(error)
    })
    .unwrap();
    let stored = load(&connection, &record.lease.task_id, &record.lease.repository)
        .unwrap()
        .unwrap();
    (stored.lease.state, calls)
}

/// ENGINE-START-LEASE-DEMOTE: a lease check that ran out of time while a slow engine
/// started keeps a Ready or RecoveryRequired lease waiting for recovery, checked again
/// later, instead of leaving it Uncertain for good.
#[test]
fn a_lease_check_that_ran_out_of_time_at_start_keeps_the_lease_for_recovery() {
    for state in [LeaseState::Ready, LeaseState::RecoveryRequired] {
        assert_eq!(
            restarted(state.clone(), &[Err("git_deadline")]),
            (LeaseState::RecoveryRequired, 1),
            "{state:?}"
        );
    }
}

/// ENGINE-START-LEASE-DEMOTE: a lease check at start that failed on a read is tried once
/// more, as a lease read's check is; a second failed read leaves the lease Uncertain.
#[test]
fn a_lease_check_that_failed_on_a_read_at_start_is_tried_once_more() {
    let missing = "No such file or directory (os error 2)";
    assert_eq!(
        restarted(LeaseState::RecoveryRequired, &[Err(missing), Ok(())]),
        (LeaseState::RecoveryRequired, 2)
    );
    assert_eq!(
        restarted(LeaseState::RecoveryRequired, &[Err(missing)]),
        (LeaseState::Uncertain, 2)
    );
}

/// ENGINE-START-LEASE-DEMOTE guard: a lease that verifies at start waits for recovery; one
/// that broke a rule is Uncertain after one check; a Reserved lease and an Uncertain one
/// on an unrenumbered volume are Uncertain without a check.
#[test]
fn a_lease_at_start_is_uncertain_on_a_rule_and_waits_for_recovery_when_it_verifies() {
    assert_eq!(
        restarted(LeaseState::Ready, &[Ok(())]),
        (LeaseState::RecoveryRequired, 1)
    );
    for rule in [
        "worktree_branch_mismatch",
        "worktree_identity_changed",
        "untrusted_git_entry:index.lock",
    ] {
        assert_eq!(
            restarted(LeaseState::RecoveryRequired, &[Err(rule)]),
            (LeaseState::Uncertain, 1),
            "{rule}"
        );
    }
    let volume = tempfile::tempdir().unwrap();
    let moved = std::fs::canonicalize(volume.path()).unwrap();
    assert_ne!(
        identity(&moved, true).unwrap().device,
        reserved_record().admission.root.device
    );
    assert_eq!(
        restarted_at(
            moved.to_str().unwrap(),
            LeaseState::Uncertain,
            &[Err("git_deadline")]
        ),
        (LeaseState::Uncertain, 1),
        "an Uncertain lease checked after a renumber stays Uncertain when its check times out"
    );
    for state in [LeaseState::Reserved, LeaseState::Uncertain] {
        assert_eq!(
            restarted(state.clone(), &[Err("git_deadline")]),
            (LeaseState::Uncertain, 0),
            "{state:?}"
        );
    }
}
