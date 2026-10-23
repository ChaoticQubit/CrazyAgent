use super::*;
use crate::engine::native_auth_host::entry::run;
use std::os::fd::OwnedFd;

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

#[tokio::test]
async fn native_auth_custodian_entry_codex_digest_mismatch_never_retires_without_effect() {
    channel_case(|case| async move {
        let (committed, launch) = case.launch("entry-codex-pin-mismatch").await;
        let executable = case.root.join("codex-pin-only");
        std::fs::write(&executable, b"pin-validation-only").unwrap();
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o700)).unwrap();
        let before = case.snapshot();
        let error = crate::engine::native_auth_host::entry::run_with_codex(
            &case.root, bootstrap_input(&case.token()), &executable, &"0".repeat(64),
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
        assert_no_effect_fact(&actual, &committed);
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
        assert_no_effect_fact(&actual, &committed);
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).unwrap();
        drop((probe, channel, launch));
    })
    .await;
}

#[tokio::test]
async fn native_auth_custodian_entry_run_session_check_rejects_real_source_or_owner_change() {
    for change in ["source", "owner"] {
        channel_case(move |case| async move {
            let (committed, launch) = case.launch("entry-run-session-stale").await;
            let mut channel = case.channel().await;
            let guard = case.guard(&committed, NativeProfileUsePurpose::Authentication);
            let session = CustodySession::new(guard);
            assert!(matches!(
                session.check(&mut channel).await.unwrap(),
                NativeAuthCustodyControl::Continue
            ));
            if change == "source" {
                std::fs::write(&case.global, format!("{CATALOG}\n")).unwrap();
            } else {
                case.host
                    .control(OwnerControl {
                        operation_id: request("entry-run-session-revoke"),
                        target: OwnerClientKind::Tui,
                        expected_epoch: case.admission().owner_epoch,
                        decision: OwnerDecision::Revoke,
                    })
                    .await
                    .unwrap();
            }
            let before = case.snapshot();
            assert!(session.check(&mut channel).await.is_err());
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
