use super::*;
use crate::conversation::ConversationMutation;
use crate::engine::owner::OwnerApplied;
use crate::rcli_host::{Control, encode_control};

fn control(session: &str, turn: &str, request: &str, text: &str) -> Control {
    serde_json::from_value(serde_json::json!({
        "protocol_version": 1,
        "session_id": session,
        "request_id": request,
        "control": {"kind": "prompt", "turn_id": turn, "text": text}
    })).unwrap()
}

fn wire_pair(session: &str, turn: &str, request: &str) -> (String, String) {
    let overhead = encode_control(&control(session, turn, request, "a")).unwrap().len() - 5;
    let ascii = 65_536 - overhead - 60_004;
    let brief = format!("{}{}", "\u{1}".repeat(5000), "b".repeat(ascii / 2));
    let message = format!("{}{}", "\u{1}".repeat(5000), "m".repeat(ascii - ascii / 2));
    assert!(brief.len() <= 8192 && message.len() < 8192);
    assert!(brief.len() + message.len() + 2 < 16_384);
    (brief, message)
}

async fn request(case: &Fixture, request: OwnerRequest) -> OwnerApplied {
    let (send, receive) = oneshot::channel();
    case.storage.sender.try_send(Job::OwnerApply(
        case.started.owners[0].admission.clone(), request, send,
    )).unwrap_or_else(|_| panic!("actual owner mutation queue unavailable"));
    receive.await.unwrap().unwrap()
}

async fn submit_request(case: &Fixture, id: &str, text: &str) -> OwnerRequest {
    let basis = owner_read(&case.storage, &case.started, OwnerRead::Capabilities).await;
    OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(id).unwrap(),
        expected_revision: basis.revision,
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Submit {
                    project_id: case.session.project_id.clone(),
                    session_id: case.session.session_id.clone(),
                    text: text.into(),
                },
            },
        },
    }
}

async fn cancel(case: &Fixture) {
    owner_apply(&case.storage, &case.started, "cancel-boundary-setup", OwnerMutation::ProjectMaster {
        action: MasterOwnerAction::Cancel {
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        },
    }).await;
}

async fn queue(case: &mut Fixture, text: &str) -> OwnerRequest {
    cancel(case).await;
    let id = "t".repeat(128);
    let submitted = submit_request(case, &id, text).await;
    assert_eq!(request(case, submitted.clone()).await.result, OwnerResult::Applied);
    let state = owner_read(&case.storage, &case.started, OwnerRead::ProjectMaster {
        project_id: case.session.project_id.clone(),
    }).await;
    let OwnerData::ProjectMaster { session: Some(session) } = state.data else {
        panic!("actual submitted session required");
    };
    let session = *session;
    let state = owner_read(&case.storage, &case.started, OwnerRead::ProjectMasterTurns {
        project_id: session.project_id.clone(), session_id: session.session_id.clone(),
        cursor: None, limit: 8,
    }).await;
    let OwnerData::ProjectMasterTurns { page } = state.data else {
        panic!("actual submitted turn required");
    };
    case.turn = page.turns.into_iter().find(|turn| turn.id.as_str() == id.as_str()).unwrap();
    case.session = session;
    assert_eq!(case.session.current_turn.as_ref(), Some(&case.turn.id));
    submitted
}

async fn brief(case: &Fixture, project: &ProjectId, version: u64, text: &str) {
    owner_apply(&case.storage, &case.started, &format!("brief-{}-{version}", project.as_str()),
        OwnerMutation::Conversation {
            mutation: ConversationMutation::ReplaceBrief {
                project_id: project.clone(), expected_version: version, text: text.into(),
            },
        },
    ).await;
}

fn assembled(case: &Fixture) -> Result<String, EngineError> {
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    prompt(&tx, &case.session, &case.turn)
}

fn counts(case: &Fixture) -> [i64; 8] {
    let db = case.db();
    ["conversation_entries", "master_turns", "master_preparations", "model_gateway_attempts",
        "repository_admissions", "native_auth_operations", "worktree_leases",
        "bootstrap_workspaces"].map(|table| {
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0)).unwrap()
        })
}

fn signals(case: &Fixture) -> (u64, i64, i64, i64) {
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    (
        owner_revision(&tx).unwrap(),
        tx.query_row("SELECT COUNT(*) FROM events", [], |row| row.get(0)).unwrap(),
        tx.query_row("SELECT COUNT(*) FROM model_gateway_observations", [], |row| row.get(0)).unwrap(),
        tx.query_row("SELECT COUNT(*) FROM owner_operations", [], |row| row.get(0)).unwrap(),
    )
}

async fn visible(case: &Fixture) -> (MasterSessionView, MasterTurnView) {
    let state = owner_read(&case.storage, &case.started, OwnerRead::ProjectMaster {
        project_id: case.session.project_id.clone(),
    }).await;
    let OwnerData::ProjectMaster { session: Some(session) } = state.data else {
        panic!("actual persisted owner-visible session required");
    };
    let session = *session;
    let state = owner_read(&case.storage, &case.started, OwnerRead::ProjectMasterTurns {
        project_id: session.project_id.clone(), session_id: session.session_id.clone(),
        cursor: None, limit: 8,
    }).await;
    let OwnerData::ProjectMasterTurns { page } = state.data else {
        panic!("actual persisted owner-visible turns required");
    };
    let turn = page.turns.into_iter().find(|turn| turn.id == case.turn.id).unwrap();
    (session, turn)
}

fn paths(root: &Path) -> Vec<PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut paths = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path.clone());
            }
            paths.push(path.strip_prefix(root).unwrap().to_path_buf());
            assert!(paths.len() < 4096);
        }
    }
    paths.sort();
    paths
}

fn stored_brief(case: &Fixture) -> String {
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let OwnerData::CurrentBrief { brief: Some(brief), .. } =
        crate::conversation::current_brief(&tx, &case.session.project_id).unwrap()
    else {
        panic!("actual saved current brief required");
    };
    brief.text
}

#[tokio::test]
async fn no_brief_preserves_exact_message_bytes() {
    let mut case = Fixture::new(false).await;
    let message = " \tmessage 🦀\r\n ";
    queue(&mut case, message).await;
    let actual = assembled(&case);
    case.close().await;
    assert_eq!(actual.unwrap(), message);
}

#[tokio::test]
async fn a_saved_owner_profile_prefaces_every_master_prompt() {
    let mut case = Fixture::new(false).await;
    let message = "What next?";
    queue(&mut case, message).await;
    let before = assembled(&case);
    let profile = crate::owner_profile::OwnerProfile {
        name: "Ada".into(),
        role: "Founder".into(),
        bio: String::new(),
        preferred_stack: vec!["Rust".into()],
        working_style: crate::owner_profile::WorkingStyle {
            detail: crate::owner_profile::AnswerDetail::Terse,
            initiative: crate::owner_profile::Initiative::ActFirst,
        },
    };
    owner_apply(&case.storage, &case.started, "save-owner-profile",
        OwnerMutation::SaveOwnerProfile { profile: profile.clone() }).await;
    let profiled = assembled(&case);
    let project = case.session.project_id.clone();
    brief(&case, &project, 0, "Build a CLI.").await;
    let briefed = assembled(&case);
    case.close().await;
    assert_eq!(before.unwrap(), message);
    let preface = "About the owner: Ada, Founder.\nPreferred stack: Rust.\nKeep answers short. Act first, then report.";
    assert_eq!(profile.preface(), preface);
    assert_eq!(profiled.unwrap(), format!("{preface}\n\n{message}"));
    assert_eq!(briefed.unwrap(), format!("{preface}\n\nBuild a CLI.\n\n{message}"));
}

#[tokio::test]
async fn latest_project_brief_uses_exact_two_lf() {
    let mut case = Fixture::new(false).await;
    let message = " \tmessage\r\n";
    queue(&mut case, message).await;
    let project = case.session.project_id.clone();
    brief(&case, &project, 0, "obsolete brief").await;
    let first = assembled(&case);
    let other = ProjectId::new("other-project").unwrap();
    owner_apply(&case.storage, &case.started, "create-other-boundary-project", OwnerMutation::CreateProject {
        project_id: other.clone(), spec: ProjectSpec { name: "Other project".into(), repositories: vec![] },
    }).await;
    brief(&case, &other, 0, "another project's newer brief").await;
    let latest = "\n current brief 🦀\t\n";
    brief(&case, &project, 1, latest).await;
    let actual = assembled(&case);
    let saved = stored_brief(&case);
    case.close().await;
    assert_eq!(first.unwrap(), format!("obsolete brief\n\n{message}"));
    assert_eq!(actual.unwrap(), format!("{latest}\n\n{message}"));
    assert_eq!(saved, latest);
}

#[tokio::test]
async fn raw_aggregate_accepts_16384_rejects_plus_one_and_two_full_entries() {
    for (brief_bytes, expected_bytes) in [(8190, 16_384), (8191, 16_385), (8192, 16_386)] {
        let mut case = Fixture::new(false).await;
        let message = "m".repeat(8192);
        let text = "b".repeat(brief_bytes);
        queue(&mut case, &message).await;
        brief(&case, &case.session.project_id, 0, &text).await;
        let actual = assembled(&case);
        let saved = stored_brief(&case);
        case.close().await;
        assert_eq!(saved, text);
        if expected_bytes == 16_384 {
            assert_eq!(actual.unwrap(), format!("{text}\n\n{message}"));
        } else {
            assert!(actual.is_err(), "raw aggregate {expected_bytes} must reject before admission");
        }
    }
}

#[tokio::test]
async fn aggregate_limits_count_utf8_bytes() {
    let mut case = Fixture::new(false).await;
    let message = "🦀".repeat(2048);
    let text = "é".repeat(4095);
    assert_eq!(message.len(), 8192);
    assert_eq!(text.len(), 8190);
    queue(&mut case, &message).await;
    brief(&case, &case.session.project_id, 0, &text).await;
    let exact = assembled(&case);
    brief(&case, &case.session.project_id, 1, &format!("{text}a")).await;
    let over = assembled(&case);
    case.close().await;
    assert_eq!(exact.unwrap(), format!("{text}\n\n{message}"));
    assert!(over.is_err(), "16385 UTF-8 bytes must reject despite fewer characters");
}

#[tokio::test]
async fn current_prompt_checks_real_encoded_payload_65536_and_plus_one() {
    let mut case = Fixture::new(false).await;
    let turn_id = "t".repeat(128);
    let (text, message) = wire_pair(case.session.session_id.as_str(), &turn_id, "rcli-control-4096");
    queue(&mut case, &message).await;
    brief(&case, &case.session.project_id, 0, &text).await;
    let expected = format!("{text}\n\n{message}");
    let exact_control = control(case.session.session_id.as_str(), &turn_id, "rcli-control-4096", &expected);
    let frame = encode_control(&exact_control).unwrap();
    let exact = assembled(&case);
    brief(&case, &case.session.project_id, 1, &format!("{text}a")).await;
    let over = assembled(&case);
    let over_control = control(case.session.session_id.as_str(), &turn_id, "rcli-control-4096",
        &format!("{text}a\n\n{message}"));
    let payload_bytes = serde_json::to_vec(&over_control).unwrap().len();
    case.close().await;
    assert_eq!(frame.len(), 65_540);
    assert_eq!(u32::from_be_bytes(frame[..4].try_into().unwrap()), 65_536);
    assert_eq!(exact.unwrap(), expected);
    assert_eq!(payload_bytes, 65_537);
    assert!(encode_control(&over_control).is_err());
    assert!(over.is_err(), "raw-valid escaped aggregate must reject beyond the actual wire cap");
}

#[test]
fn actual_control_encoder_bounds_maximum_identifiers() {
    let session = "s".repeat(128);
    let turn = "t".repeat(128);
    let request = "r".repeat(128);
    let (brief, message) = wire_pair(&session, &turn, &request);
    let text = format!("{brief}\n\n{message}");
    let value = control(&session, &turn, &request, &text);
    let frame = encode_control(&value).unwrap();
    assert_eq!(frame.len(), 65_540);
    assert_eq!(u32::from_be_bytes(frame[..4].try_into().unwrap()), 65_536);
    let decoded: Control = serde_json::from_slice(&frame[4..]).unwrap();
    assert_eq!(decoded, value);
    let recovered: serde_json::Value = serde_json::from_slice(&frame[4..]).unwrap();
    assert_eq!(recovered["control"]["text"].as_str(), Some(text.as_str()));
    let over = control(&session, &turn, &request, &format!("{text}a"));
    assert_eq!(serde_json::to_vec(&over).unwrap().len(), 65_537);
    assert!(encode_control(&over).is_err());
}

#[tokio::test]
async fn fresh_submit_rejects_size_before_new_input_or_preparation() {
    for escaped in [false, true] {
        let case = Fixture::new(false).await;
        cancel(&case).await;
        let id = "t".repeat(128);
        let (mut text, message) = if escaped {
            wire_pair(case.session.session_id.as_str(), &id, "rcli-control-4096")
        } else {
            ("b".repeat(8190), "m".repeat(8192))
        };
        text.push('a');
        brief(&case, &case.session.project_id, 0, &text).await;
        let before = counts(&case);
        let before_signals = signals(&case);
        let directories = paths(case.root.path());
        let submitted = submit_request(&case, &id, &message).await;
        let result = request(&case, submitted.clone()).await;
        let after = counts(&case);
        let after_signals = signals(&case);
        let after_paths = paths(case.root.path());
        let saved = stored_brief(&case);
        brief(&case, &case.session.project_id, 1, "now fits").await;
        let before_replay = counts(&case);
        let before_replay_signals = signals(&case);
        let replay = request(&case, submitted).await;
        let after_replay = counts(&case);
        let after_replay_signals = signals(&case);
        case.close().await;
        assert_eq!(result.result, OwnerResult::Rejected { code: "master_input_too_large".into() });
        assert!(!result.historical_replay);
        assert_eq!(result.provenance.revision_before.value(), before_signals.0);
        assert_eq!(result.provenance.revision_after, result.provenance.revision_before);
        assert_eq!(result.provenance.event_cursor, None);
        assert_eq!(result.current_revision.value(), before_signals.0);
        assert_eq!(after_signals, (before_signals.0, before_signals.1, before_signals.2, before_signals.3 + 1));
        assert_eq!(before, after, "rejection cannot insert input, turn, attempt, preparation or lease");
        assert_eq!(directories, after_paths);
        assert_eq!(saved, text);
        assert!(replay.historical_replay);
        assert_eq!(replay.result, result.result);
        assert_eq!(replay.provenance, result.provenance);
        assert_eq!(before_replay, after_replay);
        assert_eq!(before_replay_signals, after_replay_signals);
    }
}

#[tokio::test]
async fn later_oversized_brief_rejects_before_physical_bootstrap() {
    for escaped in [false, true] {
        let mut case = Fixture::new(true).await;
        let id = "t".repeat(128);
        let (mut text, message) = if escaped {
            wire_pair(case.session.session_id.as_str(), &id, "rcli-control-4096")
        } else {
            ("b".repeat(8190), "m".repeat(8192))
        };
        text.push('a');
        let submitted = queue(&mut case, &message).await;
        case.register().await;
        brief(&case, &case.session.project_id, 0, &text).await;
        let before = counts(&case);
        let before_signals = signals(&case);
        let directories = paths(case.root.path());
        let checked = assembled(&case);
        let result = case.runtime(case.prepare()).await;
        let after = counts(&case);
        let after_signals = signals(&case);
        let after_paths = paths(case.root.path());
        let (session, turn) = visible(&case).await;
        let repeat_before = case.snapshot();
        let repeated = case.runtime(case.prepare()).await;
        let repeat_after = case.snapshot();
        let repeat_signals = signals(&case);
        let replay = request(&case, submitted).await;
        let replay_counts = counts(&case);
        let replay_signals = signals(&case);
        let saved = stored_brief(&case);
        let message_saved: String = case.db().query_row(
            "SELECT text FROM conversation_entries WHERE project=?1 AND event_cursor=?2 AND kind='message'",
            params![case.session.project_id.as_str(), signed(case.turn.input_cursor.value()).unwrap()],
            |row| row.get(0),
        ).unwrap();
        case.close().await;
        assert!(checked.is_err(), "the real private prompt must reject the edited aggregate");
        let MasterRuntimeResult::Turn(returned) = result.unwrap() else {
            panic!("queued capacity rejection must return its persisted Blocked turn");
        };
        assert_eq!(*returned, turn);
        assert_eq!(turn.phase, MasterTurnPhase::Blocked);
        assert_eq!(turn.reason, Some(MasterReason::CapacityUnavailable));
        assert_eq!(turn.cause.as_deref(), Some("master_input_too_large"));
        assert!(turn.attempt.is_none() && turn.runtime.is_none() && turn.reply.is_none());
        assert!(session.current_turn.is_none());
        assert_eq!(turn.id.as_str(), id.as_str());
        assert_eq!(after_signals, (before_signals.0 + 1, before_signals.1 + 1, before_signals.2, before_signals.3));
        let MasterRuntimeResult::Turn(repeated) = repeated.unwrap() else {
            panic!("repeat Prepare must retain the Blocked turn");
        };
        assert_eq!(*repeated, turn);
        assert_eq!(repeat_before, repeat_after);
        assert_eq!(repeat_signals, after_signals);
        assert!(replay.historical_replay);
        assert_eq!(replay.result, OwnerResult::Applied);
        assert_eq!(replay_counts, after);
        assert_eq!(replay_signals, after_signals);
        assert_eq!(before, after, "rejection must precede every new admission and bootstrap reservation");
        assert_eq!(directories, after_paths, "rejection must precede even private worktree-parent creation");
        assert_eq!(saved, text);
        assert_eq!(message_saved, message);
    }
}

#[tokio::test]
async fn prepared_replay_rechecks_latest_without_new_attempt_or_workspace() {
    let message = "m".repeat(8192);
    let case = Fixture::new_with_message(true, &message).await;
    assert_eq!(case.turn.ordinal, 1);
    case.register().await;
    let prepared = match case.runtime(case.prepare()).await {
        Ok(MasterRuntimeResult::Prepared(prepared)) => {
            eprintln!("brief_prepared_setup variant=Prepared");
            Ok(prepared)
        }
        Ok(MasterRuntimeResult::Turn(turn)) => Err(format!(
            "variant=Turn ordinal={} phase={:?} reason={:?} attempt_present={}",
            turn.ordinal,
            turn.phase,
            turn.reason,
            turn.attempt.is_some(),
        )),
        Ok(MasterRuntimeResult::ConfiguredProfile(_)) => Err(String::from(
            "variant=ConfiguredProfile phase=not_applicable reason=not_applicable",
        )),
        Ok(MasterRuntimeResult::Profile(_)) => Err(String::from(
            "variant=Profile phase=not_applicable reason=not_applicable",
        )),
        Ok(MasterRuntimeResult::Checkout(_)) => Err(String::from(
            "variant=Checkout phase=not_applicable reason=not_applicable",
        )),
        Ok(MasterRuntimeResult::Waiting) => Err(String::from(
            "variant=Waiting phase=not_applicable reason=not_applicable",
        )),
        Err(_) => Err(String::from(
            "variant=Err(EngineError) phase=not_applicable reason=not_applicable",
        )),
    };
    let prepared = match prepared {
        Ok(prepared) => prepared,
        Err(observation) => {
            eprintln!("brief_prepared_setup {observation}");
            case.close().await;
            eprintln!("brief_prepared_setup close=acknowledged");
            panic!("actual configured local preparation required before edited-brief replay");
        }
    };
    brief(&case, &case.session.project_id, 0, &"b".repeat(8191)).await;
    let before = counts(&case);
    let before_signals = signals(&case);
    let directories = paths(case.root.path());
    let result = case.runtime(case.prepare()).await;
    let after = counts(&case);
    let after_signals = signals(&case);
    let after_paths = paths(case.root.path());
    let (session, turn) = visible(&case).await;
    let repeat_before = case.snapshot();
    let repeated = case.runtime(case.prepare()).await;
    let repeat_after = case.snapshot();
    let repeat_signals = signals(&case);
    case.close().await;
    assert_eq!(prepared.text, message);
    let MasterRuntimeResult::Turn(returned) = result.unwrap() else {
        panic!("prepared capacity rejection must return its genuinely completed Failed turn");
    };
    assert_eq!(*returned, turn);
    assert_eq!(turn.phase, MasterTurnPhase::Failed);
    assert_eq!(turn.reason, Some(MasterReason::CapacityUnavailable));
    assert_eq!(turn.cause.as_deref(), Some("master_input_too_large"));
    assert!(turn.runtime.is_none() && turn.reply.is_none());
    assert!(session.current_turn.is_none());
    let attempt = turn.attempt.as_ref().unwrap();
    assert_eq!(attempt.request.id, prepared.attempt.request.id);
    let completion = attempt.completion.as_ref().unwrap();
    assert_eq!(completion.effects, Effects::None);
    assert_eq!(completion.outcome, AttemptOutcome::Failed(FailureKind::Unknown));
    assert!(completion.finished_at_ms >= attempt.selected_at_ms);
    assert_eq!(completion.retry_at_ms, None);
    assert_eq!(after_signals, (before_signals.0 + 1, before_signals.1 + 1, before_signals.2, before_signals.3));
    let MasterRuntimeResult::Turn(repeated) = repeated.unwrap() else {
        panic!("repeat Prepare must retain the completed Failed turn");
    };
    assert_eq!(*repeated, turn);
    assert_eq!(repeat_before, repeat_after);
    assert_eq!(repeat_signals, after_signals);
    assert_eq!(before, after);
    assert_eq!(directories, after_paths);
}
