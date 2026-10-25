use super::*;

async fn apply_actual(
    case: &Fixture,
    owner: usize,
    id: &str,
    action: MasterOwnerAction,
) -> (OwnerRequest, OwnerApplied) {
    let basis = owner_read(&case.storage, &case.started, OwnerRead::Capabilities).await;
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: basis.revision,
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ProjectMaster { action },
        },
    };
    let applied = replay_actual(case, owner, request.clone()).await;
    (request, applied)
}

async fn replay_actual(case: &Fixture, owner: usize, request: OwnerRequest) -> OwnerApplied {
    let (send, receive) = oneshot::channel();
    case.storage.sender.try_send(Job::OwnerApply(
        case.started.owners[owner].admission.clone(), request, send,
    )).unwrap_or_else(|_| panic!("actual stop fixture owner queue unavailable"));
    receive.await.unwrap().unwrap()
}

fn result_tag(applied: &OwnerApplied) -> String {
    match &applied.result {
        OwnerResult::Applied => String::from("Applied"),
        OwnerResult::Rejected { code } => format!("Rejected code={}", code.chars().take(96).collect::<String>()),
        _ => String::from("UnexpectedNonSubmitResult"),
    }
}

async fn fresh_case(with_repository: bool, message: &str, owner: usize) -> (Fixture, OwnerRequest, OwnerApplied) {
    let mut case = Fixture::new(with_repository).await;
    let (_, closed) = apply_actual(&case, 0, "stop-close-initial", MasterOwnerAction::Close {
        project_id: case.session.project_id.clone(), session_id: case.session.session_id.clone(),
    }).await;
    if closed.result != OwnerResult::Applied || closed.historical_replay {
        case.close().await;
        eprintln!("master_stop_setup phase=initial_close result={} historical_replay={} close=acknowledged",
            result_tag(&closed), closed.historical_replay);
        panic!("actual initial session close required for fresh stop fixture");
    }
    let (_, opened) = apply_actual(&case, 0, "stop-open-session", MasterOwnerAction::Open {
        project_id: case.session.project_id.clone(), expected_config_id: case.config.id.clone(),
        expected_source_hash: case.session.source_hash.clone(), selection: Box::new(case.session.selection.clone()),
    }).await;
    if opened.result != OwnerResult::Applied || opened.historical_replay {
        case.close().await;
        eprintln!("master_stop_setup phase=open result={} historical_replay={} close=acknowledged",
            result_tag(&opened), opened.historical_replay);
        panic!("actual fresh session open required for stop fixture");
    }
    let state = owner_read(&case.storage, &case.started, OwnerRead::ProjectMaster {
        project_id: case.session.project_id.clone(),
    }).await;
    let OwnerData::ProjectMaster { session: Some(session) } = state.data else {
        case.close().await;
        panic!("actual newly opened session required for stop fixture");
    };
    let session = *session;
    case.session = session;
    let (request, applied) = apply_actual(&case, owner, "stop-fresh-submit", MasterOwnerAction::Submit {
        project_id: case.session.project_id.clone(), session_id: case.session.session_id.clone(),
        text: message.into(),
    }).await;
    if applied.result != OwnerResult::Applied || applied.historical_replay {
        case.close().await;
        eprintln!("master_stop_setup phase=submit owner_slot={owner} result={} historical_replay={} close=acknowledged",
            result_tag(&applied), applied.historical_replay);
        panic!("actual fresh applied Submit required for stop fixture");
    }
    let state = owner_read(&case.storage, &case.started, OwnerRead::ProjectMasterTurns {
        project_id: case.session.project_id.clone(), session_id: case.session.session_id.clone(),
        cursor: None, limit: 1,
    }).await;
    let OwnerData::ProjectMasterTurns { page } = state.data else {
        case.close().await;
        panic!("actual newly submitted turn required for stop fixture");
    };
    let Some(turn) = page.turns.into_iter().find(|turn| turn.id == request.request_id) else {
        case.close().await;
        panic!("actual submitted operation identity required for stop fixture");
    };
    if turn.ordinal != 1 || turn.phase != MasterTurnPhase::Queued || turn.attempt.is_some() {
        case.close().await;
        panic!("actual first queued turn required for stop fixture");
    }
    case.turn = turn;
    case.session = canonical(&case).0;
    (case, request, applied)
}

fn canonical(case: &Fixture) -> (MasterSessionView, MasterTurnView) {
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    load_runtime_turn(&tx, &case.started.host, &case.session.project_id,
        &case.session.session_id, &case.turn.id).unwrap()
}

fn settle(case: &Fixture, host: &HostStamp, binding: &MasterStopBinding) -> Result<MasterTurnView, EngineError> {
    let mut db = Connection::open_with_flags(&case.path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE | rusqlite::OpenFlags::SQLITE_OPEN_NOFOLLOW).map_err(error)?;
    db.busy_timeout(std::time::Duration::from_secs(1)).map_err(error)?;
    settle_stopped_commit(&mut db, &case.config, host, binding)
}

#[derive(Debug, PartialEq, Eq)]
struct Witness {
    snapshot: Snapshot,
    session: String,
    input: String,
    receipt: (OwnerProvenance, Vec<u8>),
    counts: [i64; 5],
    directories: Vec<PathBuf>,
}

fn witness(case: &Fixture, applied: &OwnerApplied) -> Witness {
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let counts = ["conversation_entries", "master_turns", "owner_operations", "events", "repository_admissions"]
        .map(|table| tx.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap());
    let session = tx.query_row("SELECT body FROM master_sessions WHERE id=?1",
        [case.session.session_id.as_str()], |row| row.get(0)).unwrap();
    let input = tx.query_row("SELECT text FROM conversation_entries WHERE project=?1 AND event_cursor=?2 AND kind='message'",
        params![case.session.project_id.as_str(), signed(case.turn.input_cursor.value()).unwrap()], |row| row.get(0)).unwrap();
    let receipt = owner_lookup(&tx, "native_owner", &applied.provenance.owner_id,
        &applied.provenance.operation_id, Some(&applied.provenance.request_hash)).unwrap().unwrap();
    let mut pending = vec![case.root.path().to_path_buf()];
    let mut directories = Vec::new();
    while let Some(path) = pending.pop() {
        for entry in std::fs::read_dir(&path).unwrap() {
            let entry = entry.unwrap();
            if entry.file_type().unwrap().is_dir() {
                pending.push(entry.path());
                directories.push(entry.path());
            }
        }
    }
    directories.sort();
    Witness { snapshot: case.snapshot(), session, input, receipt, counts, directories }
}

fn unchanged_inputs(before: &Witness, after: &Witness) {
    assert_eq!(after.input, before.input);
    assert_eq!(after.receipt, before.receipt);
    assert_eq!(after.snapshot.counts, before.snapshot.counts);
    assert_eq!(after.directories, before.directories);
    assert_eq!(after.counts[..3], before.counts[..3]);
    assert_eq!(after.counts[4], before.counts[4]);
}

fn one_transition(before: &Witness, after: &Witness) {
    unchanged_inputs(before, after);
    assert_eq!(after.snapshot.revision, before.snapshot.revision + 1);
    assert_eq!(after.counts[3], before.counts[3] + 1);
}

async fn bind_actual(case: Fixture, request: &OwnerRequest, applied: &OwnerApplied) -> (Fixture, MasterStopBinding) {
    match MasterStopBinding::from_fresh_submit(&case.started.host, request, applied) {
        Ok(binding) => (case, binding),
        Err(_) => {
            case.close().await;
            panic!("actual fresh Submit must bind before stop settlement; close=acknowledged");
        }
    }
}

async fn prepare_actual(case: Fixture) -> (Fixture, PreparedMasterTurn) {
    let registration = case.runtime(MasterRuntimeCommand::RegisterRcliProfile {
        host: case.started.host.clone(), project_id: case.session.project_id.clone(),
        session_id: case.session.session_id.clone(), configured_home: case.home.clone(),
    }).await;
    if !matches!(registration, Ok(MasterRuntimeResult::ConfiguredProfile(_))) {
        case.close().await;
        panic!("actual configured profile prerequisite unavailable for stop fixture");
    }
    match case.runtime(case.prepare()).await {
        Ok(MasterRuntimeResult::Prepared(prepared)) => {
            eprintln!("master_stop_setup variant=Prepared ordinal={}", case.turn.ordinal);
            (case, *prepared)
        }
        result => {
            let observation = match result {
                Ok(MasterRuntimeResult::Turn(turn)) => format!("variant=Turn ordinal={} phase={:?} reason={:?} attempt_present={}",
                    turn.ordinal, turn.phase, turn.reason, turn.attempt.is_some()),
                Ok(MasterRuntimeResult::Profile(_)) => String::from("variant=Profile"),
                Ok(MasterRuntimeResult::ConfiguredProfile(_)) => String::from("variant=ConfiguredProfile"),
                Ok(MasterRuntimeResult::Prepared(_)) => unreachable!(),
                Err(_) => String::from("variant=Err(EngineError)"),
            };
            case.close().await;
            eprintln!("master_stop_setup {observation} close=acknowledged");
            panic!("actual first-turn preparation prerequisite unavailable for stop fixture");
        }
    }
}

#[tokio::test]
async fn candidate_requires_the_actual_fresh_submit_and_unchanged_request() {
    let (case, request, applied) = fresh_case(false, PROMPT, 0).await;
    let before = witness(&case, &applied);
    let bound = MasterStopBinding::from_fresh_submit(&case.started.host, &request, &applied);
    let replay = replay_actual(&case, 0, request.clone()).await;
    let replay_bound = MasterStopBinding::from_fresh_submit(&case.started.host, &request, &replay);
    let mut changed = request.clone();
    let OwnerOperation::Apply { mutation: OwnerMutation::ProjectMaster {
        action: MasterOwnerAction::Submit { text, .. },
    } } = &mut changed.operation else { unreachable!() };
    text.push('!');
    let changed_bound = MasterStopBinding::from_fresh_submit(&case.started.host, &changed, &applied);
    let mut inconsistent = applied.clone();
    inconsistent.current_revision = Cursor::new(applied.current_revision.value() + 1);
    let inconsistent_bound = MasterStopBinding::from_fresh_submit(&case.started.host, &request, &inconsistent);
    let mut non_submit = request.clone();
    non_submit.operation = OwnerOperation::Apply { mutation: OwnerMutation::ProjectMaster {
        action: MasterOwnerAction::Close {
            project_id: case.session.project_id.clone(), session_id: case.session.session_id.clone(),
        },
    } };
    let non_submit_bound = MasterStopBinding::from_fresh_submit(&case.started.host, &non_submit, &applied);
    let mut rejected = applied.clone();
    rejected.result = OwnerResult::Rejected { code: String::from("negative-candidate") };
    let rejected_bound = MasterStopBinding::from_fresh_submit(&case.started.host, &request, &rejected);
    let after = witness(&case, &applied);
    case.close().await;
    assert!(bound.is_ok());
    assert!(replay.historical_replay);
    assert!(replay_bound.is_err());
    assert!(changed_bound.is_err());
    assert!(inconsistent_bound.is_err());
    assert!(non_submit_bound.is_err());
    assert!(rejected_bound.is_err());
    assert_eq!(after, before);
}

#[tokio::test]
async fn transaction_binds_real_host_receipt_turn_and_input_across_distinct_owners() {
    let (case, request, applied) = fresh_case(false, PROMPT, 1).await;
    let open_owner = case.session.owner.owner_id.clone();
    let (case, binding) = bind_actual(case, &request, &applied).await;
    let before = witness(&case, &applied);
    let different_hex = |value: &str| format!("{}{}", if value.starts_with('0') { "1" } else { "0" }, &value[1..]);
    let mut candidates = Vec::new();
    let mut wrong = binding.clone();
    wrong.db_instance = different_hex(&binding.db_instance);
    candidates.push(wrong);
    let mut wrong = binding.clone();
    wrong.project_id = ProjectId::new("stop-foreign-project").unwrap();
    candidates.push(wrong);
    let mut wrong = binding.clone();
    wrong.session_id = SessionId::new("stop-foreign-session").unwrap();
    candidates.push(wrong);
    let mut wrong = binding.clone();
    wrong.turn_id = RequestId::new("stop-foreign-turn").unwrap();
    candidates.push(wrong);
    let mut wrong = binding.clone();
    wrong.input_cursor = Cursor::new(binding.input_cursor.value() + 1);
    candidates.push(wrong);
    let mut wrong = binding.clone();
    wrong.provenance.owner_id = case.started.owners[0].admission.owner_id.clone();
    candidates.push(wrong);
    let mut wrong = binding.clone();
    wrong.provenance.request_hash = different_hex(&binding.provenance.request_hash);
    candidates.push(wrong);
    let mut wrong = binding.clone();
    wrong.provenance.event_cursor = Some(Cursor::new(binding.input_cursor.value() + 1));
    candidates.push(wrong);
    let rejected = candidates.iter().map(|candidate| settle(&case, &case.started.host, candidate).is_err())
        .collect::<Vec<_>>();
    let mut foreign = case.started.host.clone();
    foreign.engine_epoch = different_hex(&case.started.host.engine_epoch);
    let host_result = settle(&case, &foreign, &binding);
    let after_rejection = witness(&case, &applied);
    let settled = settle(&case, &case.started.host, &binding);
    let after = witness(&case, &applied);
    case.close().await;
    assert_ne!(open_owner, applied.provenance.owner_id);
    assert!(rejected.into_iter().all(|rejected| rejected));
    assert_eq!(host_result.err().unwrap().0, "owner_runtime_stale");
    assert_eq!(after_rejection, before);
    assert_eq!(settled.unwrap().phase, MasterTurnPhase::Cancelled);
    one_transition(&before, &after);
}

#[tokio::test]
async fn queued_settlement_is_terminal_idempotent_and_preserves_a_newer_current_turn() {
    let (case, request, applied) = fresh_case(false, PROMPT, 0).await;
    let (case, binding) = bind_actual(case, &request, &applied).await;
    let before = witness(&case, &applied);
    let settled = settle(&case, &case.started.host, &binding);
    let after = witness(&case, &applied);
    let (session, turn) = canonical(&case);
    let repeated = settle(&case, &case.started.host, &binding);
    let repeat = witness(&case, &applied);
    let (_, newer) = apply_actual(&case, 0, "stop-newer-submit", MasterOwnerAction::Submit {
        project_id: case.session.project_id.clone(), session_id: case.session.session_id.clone(),
        text: String::from("actual later queued turn"),
    }).await;
    let newer_before = witness(&case, &applied);
    let old_again = settle(&case, &case.started.host, &binding);
    let newer_after = witness(&case, &applied);
    let current = canonical(&case).0.current_turn;
    case.close().await;
    let settled = settled.unwrap();
    assert_eq!(settled.phase, MasterTurnPhase::Cancelled);
    assert_eq!(settled.reason, Some(MasterReason::Cancelled));
    assert!(settled.attempt.is_none() && settled.runtime.is_none() && settled.reply.is_none());
    assert_eq!(turn, settled);
    assert_eq!(session.current_turn, None);
    one_transition(&before, &after);
    assert_eq!(repeated.unwrap(), settled);
    assert_eq!(repeat, after);
    assert_eq!(newer.result, OwnerResult::Applied);
    assert!(!newer.historical_replay);
    assert_eq!(current, Some(newer.provenance.operation_id));
    assert_eq!(old_again.unwrap(), settled);
    assert_eq!(newer_after, newer_before);
}

#[tokio::test]
async fn genuine_prepared_settlement_completes_its_existing_attempt_with_no_effects() {
    let (case, request, applied) = fresh_case(true, PROMPT, 0).await;
    let (case, binding) = bind_actual(case, &request, &applied).await;
    let (case, prepared) = prepare_actual(case).await;
    let before = witness(&case, &applied);
    let settled = settle(&case, &case.started.host, &binding);
    let after = witness(&case, &applied);
    let (session, turn) = canonical(&case);
    let repeat = settle(&case, &case.started.host, &binding);
    let repeated = witness(&case, &applied);
    case.close().await;
    let settled = settled.unwrap();
    let attempt = settled.attempt.as_ref().unwrap();
    let completion = attempt.completion.as_ref().unwrap();
    assert_eq!(prepared.session.current_turn.as_ref(), Some(&settled.id));
    assert_eq!(settled.phase, MasterTurnPhase::Cancelled);
    assert_eq!(settled.reason, Some(MasterReason::Cancelled));
    assert_eq!(attempt.request, prepared.attempt.request);
    assert_eq!(attempt.selected_at_ms, prepared.attempt.selected_at_ms);
    assert_eq!(completion.outcome, AttemptOutcome::Failed(FailureKind::Unknown));
    assert_eq!(completion.effects, Effects::None);
    assert_eq!(completion.retry_at_ms, None);
    assert!(completion.finished_at_ms >= attempt.selected_at_ms);
    assert!(settled.runtime.is_none() && settled.reply.is_none());
    assert_eq!(turn, settled);
    assert_eq!(session.current_turn, None);
    one_transition(&before, &after);
    assert_eq!(repeat.unwrap(), settled);
    assert_eq!(repeated, after);
}

#[tokio::test]
async fn genuine_delivering_settlement_preserves_uncertainty_and_actual_attempt_identity() {
    let (case, request, applied) = fresh_case(true, PROMPT, 0).await;
    let (case, binding) = bind_actual(case, &request, &applied).await;
    let (case, prepared) = prepare_actual(case).await;
    let acquired = case.runtime(MasterRuntimeCommand::AcquireProfile {
        host: case.started.host.clone(), project_id: case.session.project_id.clone(),
        session_id: case.session.session_id.clone(), turn_id: case.turn.id.clone(),
        attempt_id: prepared.attempt.request.id.clone(),
    }).await;
    let guard = match acquired {
        Ok(MasterRuntimeResult::Profile(guard)) => guard,
        _ => {
            case.close().await;
            panic!("actual Execution profile guard prerequisite unavailable for stop fixture");
        }
    };
    let delivering = canonical(&case).1;
    let before = witness(&case, &applied);
    let settled = settle(&case, &case.started.host, &binding);
    let after = witness(&case, &applied);
    let (session, turn) = canonical(&case);
    let repeat = settle(&case, &case.started.host, &binding);
    let repeated = witness(&case, &applied);
    drop(guard);
    case.close().await;
    let settled = settled.unwrap();
    let attempt = settled.attempt.as_ref().unwrap();
    assert_eq!(delivering.phase, MasterTurnPhase::Delivering);
    assert_eq!(settled.phase, MasterTurnPhase::Uncertain);
    assert_eq!(settled.reason, Some(MasterReason::CaptureIncomplete));
    assert_eq!(attempt.request, prepared.attempt.request);
    assert_eq!(attempt.selected_at_ms, prepared.attempt.selected_at_ms);
    assert_eq!(attempt.completion.as_ref().unwrap().effects, Effects::Uncertain);
    assert_eq!(attempt.completion.as_ref().unwrap().outcome, AttemptOutcome::Failed(FailureKind::Unknown));
    assert!(settled.runtime.is_none() && settled.reply.is_none());
    assert_eq!(session.current_turn.as_ref(), Some(&settled.id));
    assert_eq!(turn, settled);
    one_transition(&before, &after);
    assert_eq!(repeat.unwrap(), settled);
    assert_eq!(repeated, after);
}

#[tokio::test]
async fn genuine_blocked_and_locally_failed_turns_are_unchanged_terminal_settlements() {
    let blocked_message = "m".repeat(8192);
    let (case, request, applied) = fresh_case(true, &blocked_message, 0).await;
    let (case, binding) = bind_actual(case, &request, &applied).await;
    case.register().await;
    owner_apply(&case.storage, &case.started, "stop-oversized-queued-brief", OwnerMutation::Conversation {
        mutation: ConversationMutation::ReplaceBrief {
            project_id: case.session.project_id.clone(), expected_version: 0, text: "b".repeat(8191),
        },
    }).await;
    let blocked = case.runtime(case.prepare()).await;
    let before = witness(&case, &applied);
    let settled = settle(&case, &case.started.host, &binding);
    let after = witness(&case, &applied);
    case.close().await;
    let MasterRuntimeResult::Turn(blocked) = blocked.unwrap() else { panic!("actual blocked turn required") };
    assert_eq!(blocked.phase, MasterTurnPhase::Blocked);
    assert_eq!(settled.unwrap(), *blocked);
    assert_eq!(after, before);

    let message = "m".repeat(8192);
    let (case, request, applied) = fresh_case(true, &message, 0).await;
    let (case, binding) = bind_actual(case, &request, &applied).await;
    let (case, prepared) = prepare_actual(case).await;
    owner_apply(&case.storage, &case.started, "stop-oversized-latest-brief", OwnerMutation::Conversation {
        mutation: ConversationMutation::ReplaceBrief {
            project_id: case.session.project_id.clone(), expected_version: 0, text: "b".repeat(8191),
        },
    }).await;
    let failed = case.runtime(case.prepare()).await;
    let before = witness(&case, &applied);
    let settled = settle(&case, &case.started.host, &binding);
    let after = witness(&case, &applied);
    case.close().await;
    let MasterRuntimeResult::Turn(failed) = failed.unwrap() else { panic!("actual local capacity failure required") };
    assert_eq!(failed.phase, MasterTurnPhase::Failed);
    assert_eq!(failed.reason, Some(MasterReason::CapacityUnavailable));
    assert_eq!(failed.attempt.as_ref().unwrap().request, prepared.attempt.request);
    assert_eq!(failed.attempt.as_ref().unwrap().completion.as_ref().unwrap().effects, Effects::None);
    assert_eq!(settled.unwrap(), *failed);
    assert_eq!(after, before);
}

#[tokio::test]
async fn actual_owner_offline_rejects_settlement_without_rewriting_the_committed_turn() {
    let (case, request, applied) = fresh_case(false, PROMPT, 0).await;
    let (case, binding) = bind_actual(case, &request, &applied).await;
    let before = witness(&case, &applied);
    let (send, receive) = oneshot::channel();
    case.storage.sender.try_send(Job::OwnerStop(case.started.host.clone(), send))
        .unwrap_or_else(|_| panic!("actual stop fixture OwnerStop queue unavailable"));
    let stopped = receive.await.unwrap();
    let result = settle(&case, &case.started.host, &binding);
    let after = witness(&case, &applied);
    let (send, receive) = oneshot::channel();
    case.storage.sender.try_send(Job::Close(send))
        .unwrap_or_else(|_| panic!("actual stop fixture Close queue unavailable"));
    let closed = receive.await.unwrap();
    assert!(stopped.is_ok());
    assert!(closed.is_ok());
    assert_eq!(result.err().unwrap().0, "owner_runtime_stale");
    assert_eq!(after, before);
}

#[tokio::test]
async fn a_turn_submitted_after_a_stopped_prepared_turn_is_admitted_on_its_own_attempt_chain() {
    let (case, request, applied) = fresh_case(true, PROMPT, 0).await;
    let (case, binding) = bind_actual(case, &request, &applied).await;
    let (mut case, first) = prepare_actual(case).await;
    let stopped = settle(&case, &case.started.host, &binding);
    let (second, submitted) = apply_actual(&case, 0, "stop-second-submit", MasterOwnerAction::Submit {
        project_id: case.session.project_id.clone(), session_id: case.session.session_id.clone(),
        text: PROMPT.into(),
    }).await;
    let state = owner_read(&case.storage, &case.started, OwnerRead::ProjectMasterTurns {
        project_id: case.session.project_id.clone(), session_id: case.session.session_id.clone(),
        cursor: None, limit: 2,
    }).await;
    let OwnerData::ProjectMasterTurns { page } = state.data else {
        case.close().await;
        panic!("actual turn page required after the second submit");
    };
    let Some(turn) = page.turns.into_iter().find(|turn| turn.id == second.request_id) else {
        case.close().await;
        panic!("actual second submitted turn required");
    };
    case.turn = turn;
    case.session = canonical(&case).0;
    let ordinal = case.turn.ordinal;
    let admitted = case.runtime(case.prepare()).await;
    case.close().await;
    assert_eq!(stopped.unwrap().phase, MasterTurnPhase::Cancelled);
    assert_eq!(result_tag(&submitted), "Applied");
    assert_eq!(ordinal, 2);
    match admitted {
        Ok(MasterRuntimeResult::Prepared(prepared)) => {
            assert_eq!(prepared.session.current_turn.as_ref(), Some(&second.request_id));
            assert_ne!(prepared.attempt.request.id, first.attempt.request.id);
            assert_ne!(prepared.attempt.request.task_id, first.attempt.request.task_id);
            assert!(prepared.attempt.request.previous_attempt.is_none());
            assert!(prepared.attempt.completion.is_none());
        }
        Ok(MasterRuntimeResult::Turn(turn)) => {
            panic!("second turn not admitted: phase={:?} reason={:?}", turn.phase, turn.reason)
        }
        Ok(_) => panic!("second turn: another runtime result"),
        Err(failure) => panic!("second turn: {}", failure.0),
    }
}
