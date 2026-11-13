use super::*;
use crate::engine::native_auth_host::entry::run;
use std::os::fd::OwnedFd;
use std::os::unix::process::CommandExt;

fn bootstrap_input(bytes: &[u8]) -> OwnedFd {
    assert!(bytes.len() <= 33);
    let (reader, mut writer) = std::io::pipe().unwrap();
    std::io::Write::write_all(&mut writer, bytes).unwrap();
    drop(writer);
    reader.into()
}

fn assert_no_effect_fact(actual: &NativeAuthFact, committed: &NativeAuthFact) {
    assert_eq!(actual.operation_id, committed.operation_id);
    assert_eq!(actual.generation, committed.generation);
    assert_eq!(actual.binding, committed.binding);
    assert_eq!(actual.action, committed.action);
    assert_eq!(actual.phase, NativeAuthPhase::Unavailable);
    assert_eq!(actual.code.as_deref(), Some("native_auth_host_unavailable"));
}

/// Checks that a childless retire after the owner's cancel settled the committed attempt
/// Cancelled with no code (ND1-CDX-2, leader 08:51). Inputs: the retired fact and the
/// committed one. Output: none; panics on a difference.
fn assert_cancelled_without_effect(actual: &NativeAuthFact, committed: &NativeAuthFact) {
    assert_eq!(actual.operation_id, committed.operation_id);
    assert_eq!(actual.generation, committed.generation);
    assert_eq!(actual.binding, committed.binding);
    assert_eq!(actual.action, committed.action);
    assert_eq!(actual.phase, NativeAuthPhase::Cancelled);
    assert_eq!(actual.code, None);
}

#[tokio::test]
async fn native_auth_custodian_entry_codex_digest_mismatch_never_retires_without_effect() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-codex-pin-mismatch").await;
        let executable = case.root.join("codex-pin-only");
        std::fs::write(&executable, b"pin-validation-only").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let before = case.snapshot();
        let error = crate::engine::native_auth_host::entry::run_with_codex(
            &case.root, bootstrap_input(&case.token()), &executable, &"0".repeat(64), &[],
        ).await.unwrap_err();
        assert_eq!(error.0, "native_auth_host_executable_changed");
        let after = case.snapshot();
        assert_eq!(after.fences, before.fences);
        assert_eq!(after.facts[0].operation_id, committed.operation_id);
        assert_ne!(after.facts[0].phase, NativeAuthPhase::Unavailable);
        let probe = case.probe();
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        drop((probe, launch));
    }).await;
}

#[tokio::test]
async fn native_auth_custodian_entry_run_retires_real_committed_work_and_preserves_replay() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-run-sign-in").await;
        let token = case.token();
        let actual = run(&case.root, bootstrap_input(&token)).await.unwrap();
        assert_no_effect_fact(&actual, &committed);
        let after = case.snapshot();
        assert_eq!(after.facts, vec![actual.clone()]);
        assert!(after.fences.is_empty());
        let probe = case.probe();
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::Unlock).unwrap();
        drop(probe);
        let replay = case
            .client(OwnerClientKind::Tui)
            .start_native_auth(case.start("entry-run-sign-in"))
            .await
            .unwrap();
        assert_eq!(replay.fact, actual);
        assert!(replay.account.is_none());
        assert!(replay.challenge.is_none());
        assert!(run(&case.root, bootstrap_input(&token)).await.is_err());
        assert_eq!(case.snapshot(), after);
        drop(launch);
    })
    .await;
}

#[tokio::test]
async fn native_auth_custodian_entry_run_logout_preserves_actual_action_and_replay() {
    channel_case(|case| async move {
        let original = NativeAuthLogout {
            request_id: request("entry-run-logout"),
            scope: case.start("entry-run-logout-scope").scope,
        };
        let begun = case.begin(Command::Logout(original.clone())).await;
        let committed = begun.reply.fact;
        assert_eq!(committed.action, NativeAuthAction::Logout);
        let launch = case
            .host
            .prepare_native_auth_custody(begun.work.unwrap())
            .await
            .unwrap();
        let actual = run(&case.root, bootstrap_input(&case.token())).await.unwrap();
        assert_no_effect_fact(&actual, &committed);
        let replay = case
            .client(OwnerClientKind::Tui)
            .logout_native_auth(original)
            .await
            .unwrap();
        assert_eq!(replay.fact, actual);
        assert!(replay.account.is_none());
        assert!(replay.challenge.is_none());
        assert_eq!(case.snapshot().facts, vec![actual]);
        assert!(case.snapshot().fences.is_empty());
        drop(launch);
    })
    .await;
}

#[tokio::test]
async fn native_auth_custodian_entry_run_retains_physical_guard_through_commit_and_ack() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-run-commit-ack").await;
        let mut checkpoint = retirement_checkpoint(&case, &committed);
        let input = bootstrap_input(&case.token());
        let mut future = Box::pin(run(&case.root, input));
        retirement_write_started(&mut future, &mut checkpoint).await;
        let probe = case.probe();
        assert!(
            rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_err()
        );
        assert_eq!(case.snapshot().facts, vec![committed.clone()]);
        assert_eq!(case.snapshot().fences.len(), 1);
        checkpoint.release().unwrap();
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let observed = case.snapshot();
                if observed.fences.is_empty() {
                    assert_no_effect_fact(&observed.facts[0], &committed);
                    break;
                }
                assert_eq!(observed.facts, vec![committed.clone()]);
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_err()
        );
        let actual = future.await.unwrap();
        assert_no_effect_fact(&actual, &committed);
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        drop((probe, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_custodian_entry_run_truncated_bootstrap_leaves_real_capability_unused() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-run-short-bootstrap").await;
        let token = case.token();
        let before = case.snapshot();
        let error = run(&case.root, bootstrap_input(&token[..31])).await.unwrap_err();
        assert_eq!(error.0, "native_auth_bootstrap_truncated");
        assert_eq!(case.snapshot(), before);
        let mut channel = case.channel().await;
        assert_eq!(channel.fact(), &committed);
        let guard = case.guard(&committed, NativeProfileUsePurpose::Authentication);
        let session = CustodySession::new(guard);
        assert!(matches!(
            session.check(&mut channel).await.unwrap(),
            NativeAuthCustodyControl::Continue
        ));
        let actual = channel
            .retire(session.seal_without_effect().unwrap())
            .await
            .unwrap();
        assert_no_effect_fact(&actual, &committed);
        assert!(case.snapshot().fences.is_empty());
        drop((channel, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_custodian_entry_run_rejects_occupied_physical_profile_without_retirement() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-run-occupied-profile").await;
        let retained = case.guard(&committed, NativeProfileUsePurpose::Authentication);
        let before = case.snapshot();
        assert!(run(&case.root, bootstrap_input(&case.token())).await.is_err());
        assert_eq!(case.snapshot().fences, before.fences);
        assert_ne!(case.snapshot().facts[0].phase, NativeAuthPhase::Unavailable);
        let probe = case.probe();
        assert!(
            rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_err()
        );
        drop(retained);
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        drop((probe, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_custodian_entry_run_honors_committed_cancel_before_attachment() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-run-pre-attach-cancel").await;
        let cancelled = case
            .client(OwnerClientKind::Tui)
            .cancel_native_auth(NativeAuthCancel {
                request_id: request("entry-run-commit-cancel"),
                scope: case.start("entry-run-cancel-scope").scope,
                operation_id: committed.operation_id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(cancelled.fact.phase, NativeAuthPhase::Cancelling);
        assert_eq!(case.snapshot().fences.len(), 1);
        let actual = run(&case.root, bootstrap_input(&case.token())).await.unwrap();
        assert_cancelled_without_effect(&actual, &committed);
        assert_eq!(case.snapshot().facts, vec![actual]);
        assert!(case.snapshot().fences.is_empty());
        drop(launch);
    })
    .await;
}

#[tokio::test]
async fn native_auth_custodian_entry_run_session_check_reads_real_cancel_and_retains_guard() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-run-session-check").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&committed, NativeProfileUsePurpose::Authentication);
        let session = CustodySession::new(guard);
        assert!(matches!(
            session.check(&mut channel).await.unwrap(),
            NativeAuthCustodyControl::Continue
        ));
        let cancelled = case
            .client(OwnerClientKind::Tui)
            .cancel_native_auth(NativeAuthCancel {
                request_id: request("entry-run-session-cancel"),
                scope: case.start("entry-run-session-cancel-scope").scope,
                operation_id: committed.operation_id.clone(),
            })
            .await
            .unwrap();
        assert_eq!(cancelled.fact.phase, NativeAuthPhase::Cancelling);
        assert!(matches!(
            session.check(&mut channel).await.unwrap(),
            NativeAuthCustodyControl::Cancel
        ));
        let probe = case.probe();
        assert!(
            rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .is_err()
        );
        assert_eq!(case.snapshot().fences.len(), 1);
        let actual = channel
            .retire(session.seal_without_effect().unwrap())
            .await
            .unwrap();
        assert_cancelled_without_effect(&actual, &committed);
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        drop((probe, channel, launch));
    })
    .await;
}

/// A custodian's session check refuses a real change of its own entry or a revoked owner.
/// The engine ends the custody session on a refused own-entry check, so the custodian reads
/// the channel's end; the engine's refusal code itself is pinned by `native_auth_custody_tests`.
#[tokio::test]
async fn native_auth_custodian_entry_run_session_check_rejects_real_source_or_owner_change() {
    for change in ["entry", "owner", "bytes"] {
        channel_case(move |case| async move {
            let (committed, launch) = case.launch("entry-run-session-stale").await;
            let mut channel = case.channel().await;
            let guard = case.guard(&committed, NativeProfileUsePurpose::Authentication);
            let session = CustodySession::new(guard);
            assert!(matches!(
                session.check(&mut channel).await.unwrap(),
                NativeAuthCustodyControl::Continue
            ));
            case.change(change, "entry-run-session-revoke").await;
            if change == "bytes" {
                assert!(matches!(
                    session.check(&mut channel).await.unwrap(),
                    NativeAuthCustodyControl::Continue
                ));
                drop(session);
                drop((channel, launch));
                return;
            }
            let before = case.snapshot();
            let refused = session.check(&mut channel).await.err().unwrap();
            if change == "entry" {
                assert_eq!(refused.0, "unexpected end of file");
            }
            let probe = case.probe();
            assert!(
                rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                    .is_err()
            );
            assert_eq!(case.snapshot().fences, before.fences);
            assert_ne!(case.snapshot().facts[0].phase, NativeAuthPhase::Unavailable);
            drop(session);
            rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive)
                .unwrap();
            drop((probe, channel, launch));
        })
        .await;
    }
}

/// Starts a sign-in on the case's real owner and seals its custody channel as in row 3
/// (N-E1): the profile's home is replaced, so the session's next check refuses it with
/// `native_profile_custody_unsafe` and closes the channel. Input: the case. Output: the
/// launch, the sealed channel and the session.
async fn sealed_by_a_replaced_home(
    case: &ChannelCase,
) -> (
    NativeAuthCustodyLaunch,
    NativeAuthCustodyChannel,
    CustodySession,
) {
    let (committed, launch) = case.launch("entry-run-sealed-cause").await;
    let mut channel = case.channel().await;
    let guard = case.guard(&committed, NativeProfileUsePurpose::Authentication);
    let session = CustodySession::new(guard);
    let home = session.profile_path().unwrap();
    std::fs::rename(&home, case.root.join("replaced-home")).unwrap();
    std::fs::create_dir(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
    let sealed = session.check(&mut channel).await.err().map(|error| error.0);
    assert_eq!(sealed.as_deref(), Some("native_profile_custody_unsafe"));
    (launch, channel, session)
}

/// Row 3 (N-E1), Claude Code: on a channel a refused replaced home sealed, `finish_failure`
/// returns that cause, `native_profile_custody_unsafe`, not the sealed channel's
/// `native_auth_recovery_required`, so the engine records the listed custodian code.
#[tokio::test]
async fn native_auth_custodian_claude_failure_report_keeps_the_cause_that_sealed_its_channel() {
    channel_case(|case| async move {
        let (launch, mut channel, session) = sealed_by_a_replaced_home(&case).await;
        let reported = crate::engine::native_auth_host::claude_auth::finish_failure(
            session,
            &mut channel,
            EngineError("native_profile_custody_unsafe".into()),
        )
        .await
        .err();
        assert_eq!(
            reported.map(|error| error.0).as_deref(),
            Some("native_profile_custody_unsafe")
        );
        drop((channel, launch));
    })
    .await;
}

/// Row 3 (N-E1), Codex: on a channel a refused replaced home sealed, `observe_failure`
/// returns that cause, `native_profile_custody_unsafe`, not the sealed channel's
/// `native_auth_recovery_required`, so the engine records the listed custodian code. As in
/// `codex_auth::run`, the session's started child is reaped before the report.
#[tokio::test]
async fn native_auth_custodian_codex_failure_report_keeps_the_cause_that_sealed_its_channel() {
    channel_case(|case| async move {
        let (launch, mut channel, mut session) = sealed_by_a_replaced_home(&case).await;
        let child = crate::spawn_gate::spawn(
            ProcessCommand::new("/usr/bin/true")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0),
        )
        .unwrap();
        if let Err((error, mut child)) = session.retain_child(child) {
            let _ = child.wait();
            panic!("retained child rejected: {error}");
        }
        assert!(session.child_mut().unwrap().wait().unwrap().success());
        assert!(session.child_attempted());
        let reported = crate::engine::native_auth_host::codex_auth::observe_failure(
            &session,
            &mut channel,
            EngineError("native_profile_custody_unsafe".into()),
            None,
        )
        .await
        .err();
        assert_eq!(
            reported.map(|error| error.0).as_deref(),
            Some("native_profile_custody_unsafe")
        );
        drop((session, channel, launch));
    })
    .await;
}

/// Row 12 r1 F2, Codex: the engine log write is best effort. When the engine refuses the
/// log, `observe_failure` still returns the Codex failure it was given, not the log
/// exchange's error, so the custodian's cause stays the one that ended the sign-in.
#[tokio::test]
async fn native_auth_custodian_codex_failure_report_keeps_its_cause_when_the_log_is_refused() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-run-log-cause").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&committed, NativeProfileUsePurpose::Authentication);
        let mut session = CustodySession::new(guard);
        let child = crate::spawn_gate::spawn(
            ProcessCommand::new("/usr/bin/true")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0),
        )
        .unwrap();
        if let Err((error, mut child)) = session.retain_child(child) {
            let _ = child.wait();
            panic!("retained child rejected: {error}");
        }
        assert!(session.child_mut().unwrap().wait().unwrap().success());
        let refused = crate::engine::owner::native_auth::CodexLog {
            dropped: Vec::new(),
            stderr: "line\nbreak".into(),
        };
        assert!(refused.validate().is_err());
        let reported = crate::engine::native_auth_host::codex_auth::observe_failure(
            &session,
            &mut channel,
            EngineError("native_auth_codex_deadline".into()),
            Some(refused),
        )
        .await
        .err();
        assert_eq!(
            reported.map(|error| error.0).as_deref(),
            Some("native_auth_codex_deadline")
        );
        drop((session, channel, launch));
    })
    .await;
}

/// Row 12 r2 L2, Codex: a profile guard the engine log write is the first to refuse is the
/// custodian's cause. The profile's home is replaced after the sign-in ends, so the log
/// write's guard check refuses it and seals the channel, and `observe_failure` returns that
/// guard's code, `native_profile_custody_unsafe`, not the Codex failure it was given.
#[tokio::test]
async fn native_auth_custodian_codex_failure_report_keeps_a_guard_refusal_seen_at_the_log() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-run-log-guard").await;
        let mut channel = case.channel().await;
        let guard = case.guard(&committed, NativeProfileUsePurpose::Authentication);
        let mut session = CustodySession::new(guard);
        let child = crate::spawn_gate::spawn(
            ProcessCommand::new("/usr/bin/true")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0),
        )
        .unwrap();
        if let Err((error, mut child)) = session.retain_child(child) {
            let _ = child.wait();
            panic!("retained child rejected: {error}");
        }
        assert!(session.child_mut().unwrap().wait().unwrap().success());
        let home = session.profile_path().unwrap();
        std::fs::rename(&home, case.root.join("replaced-home")).unwrap();
        std::fs::create_dir(&home).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        let log = crate::engine::owner::native_auth::CodexLog {
            dropped: Vec::new(),
            stderr: "codex stopped".into(),
        };
        assert!(log.validate().is_ok());
        assert!(!channel.sealed());
        let reported = crate::engine::native_auth_host::codex_auth::observe_failure(
            &session,
            &mut channel,
            EngineError("native_auth_codex_deadline".into()),
            Some(log),
        )
        .await
        .err();
        assert!(channel.sealed());
        assert_eq!(
            reported.map(|error| error.0).as_deref(),
            Some("native_profile_custody_unsafe")
        );
        drop((session, channel, launch));
    })
    .await;
}
