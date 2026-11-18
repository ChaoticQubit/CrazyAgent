/// PA-6 (DEC-091): an RCLI master moves to the next key of its pool after a limit, as
/// the other masters do: the limit pauses the first account and the next turn runs on
/// the second key under that key's own profile and account.
#[tokio::test]
async fn an_rcli_master_moves_to_the_next_pool_key_after_a_limit() {
    let case = Fixture::pooled().await;
    case.register().await;
    generated_home(&case, &spare_connection(), "http://127.0.0.1:11435/v1");
    let MasterRuntimeResult::Prepared(first) = case.runtime(case.prepare()).await.unwrap() else {
        panic!("the owner's chosen key must prepare the first turn");
    };
    assert_eq!(first.attempt.connection.id, reference("local-connection"));
    assert_eq!(
        first.attempt.request.target_profile_ref,
        reference("local-profile")
    );
    fail_first(&case, &first, MasterReason::RateLimited).await;
    let paused: bool = case
        .db()
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM team_paused_accounts WHERE connection='local-connection')",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(paused, "a limit pauses the key that hit it");
    let prepare = next_turn(&case, "after-limit").await;
    let MasterRuntimeResult::Prepared(second) = case.runtime(prepare).await.unwrap() else {
        panic!("the pool's second key must prepare the next turn");
    };
    assert_eq!(second.attempt.connection, spare_connection());
    assert_eq!(second.attempt.request.target_profile_ref, spare_profile());
    assert_eq!(
        second.attempt.request.route.account_ref,
        reference("spare-account")
    );
    assert_eq!(second.session.session_id, case.session.session_id);
    let registered: i64 = case
        .db()
        .query_row(
            "SELECT COUNT(*) FROM configured_master_profiles WHERE session=?1",
            [case.session.session_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        registered, 2,
        "each key of the pool holds its own registration"
    );
    case.close().await;
}

/// PA-6 (DEC-092): an RCLI master keeps its conversation when settings outside it
/// change, as the other masters do: the next send moves the session to the new settings
/// and the turn runs on its registration moved there from custody's unchanged profile.
#[tokio::test]
async fn an_rcli_master_keeps_its_conversation_after_a_settings_change_outside_it() {
    let case = Fixture::kept().await;
    let registered = case.register().await;
    let (before, prepare) = rebind_after_outside_edit(&case).await;
    let (session, _) = case.view().await;
    assert_eq!(session.session_id, case.session.session_id);
    assert_eq!(session.rebound.len(), 1);
    assert!(session.rebinds_valid());
    assert_ne!(session.binding().0, &before);
    let MasterRuntimeResult::Prepared(second) = case.runtime(prepare).await.unwrap() else {
        panic!("the rebound RCLI master must prepare its next turn");
    };
    assert_eq!(&second.attempt.config_id, session.binding().0);
    assert_eq!(second.attempt.connection, registered.connection);
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let record = registration(
        &tx,
        &case.session.session_id,
        &case.session.selection.target_profile_ref,
    )
    .unwrap()
    .unwrap();
    assert_ne!(record.proof.registration_id, registered.registration_id);
    assert_eq!(&record.proof.config_id, session.binding().0);
    assert_eq!(record.proof.custody_id, registered.custody_id);
    drop(tx);
    case.close().await;
}

/// PA-6: a rebound RCLI master reads each turn under a binding it held and no other: a
/// live turn needs the current binding, a settled one may keep the Open binding, and a
/// pair the session never held is refused, also by the turn's preparation check.
#[tokio::test]
async fn a_rebound_rcli_master_admits_each_turn_only_under_a_binding_it_held() {
    let case = Fixture::kept().await;
    case.register().await;
    let (before, prepare) = rebind_after_outside_edit(&case).await;
    let MasterRuntimeResult::Prepared(_) = case.runtime(prepare).await.unwrap() else {
        panic!("the rebound RCLI master must prepare its next turn");
    };
    let (session, turns) = case.view().await;
    let open = (&case.session.config_id, case.session.source_hash.as_str());
    let now = session.binding();
    assert_eq!(open.0, &before);
    assert_ne!(now, open);
    assert!(session.admits_binding(now.0, now.1, true));
    assert!(session.admits_binding(now.0, now.1, false));
    assert!(
        session.admits_binding(open.0, open.1, false),
        "a settled turn keeps the Open binding"
    );
    assert!(
        !session.admits_binding(open.0, open.1, true),
        "a live turn needs the current binding"
    );
    assert!(!session.admits_binding(now.0, open.1, false));
    assert!(!session.admits_binding(open.0, now.1, false));
    let settled = turns
        .iter()
        .find(|turn| turn.phase == MasterTurnPhase::Failed)
        .expect("the first turn settled before the edit");
    let live = turns
        .iter()
        .find(|turn| turn.phase == MasterTurnPhase::Prepared)
        .expect("the turn after the rebind is prepared");
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    assert!(preparation(&tx, &session, settled).is_ok());
    assert!(preparation(&tx, &session, live).is_ok());
    let refused = |turn: &MasterTurnView| {
        preparation(&tx, &session, turn)
            .err()
            .map(|failure| failure.0)
    };
    let mut stale = settled.clone();
    for phase in [
        MasterTurnPhase::Prepared,
        MasterTurnPhase::Delivering,
        MasterTurnPhase::Running,
    ] {
        stale.phase = phase;
        assert_eq!(
            refused(&stale).as_deref(),
            Some("master_preparation_record"),
            "a live turn on the Open binding is refused ({phase:?})"
        );
    }
    assert_ne!(now.0, open.0, "the outside edit moved the config id");
    let mut foreign = settled.clone();
    foreign
        .attempt
        .as_mut()
        .expect("the settled turn keeps its attempt")
        .config_id = now.0.clone();
    assert_eq!(
        refused(&foreign).as_deref(),
        Some("master_preparation_record"),
        "a proof whose config is not its own attempt's is refused"
    );
    let mut unbound = session.clone();
    unbound.rebound.clear();
    assert_eq!(
        preparation(&tx, &unbound, live)
            .err()
            .map(|failure| failure.0)
            .as_deref(),
        Some("master_preparation_record"),
        "a live turn prepared on a binding the session does not hold now is refused"
    );
    drop(tx);
    case.close().await;
}

#[tokio::test]
async fn an_added_folder_project_prepares_its_master_turn_on_its_own_repository() {
    let case = Fixture::onboarded().await;
    case.register().await;
    let prepared = match case.runtime(case.prepare()).await {
        Ok(MasterRuntimeResult::Prepared(prepared)) => prepared,
        Ok(_) => panic!("an added folder must prepare its master turn"),
        Err(failure) => panic!("an added folder must reach its master turn: {failure:?}"),
    };
    assert_eq!(prepared.text, PROMPT);
    assert_eq!(prepared.repository, case.repository);
    assert_eq!(prepared.lease.state, LeaseState::Ready);
    assert!(prepared.cwd.join(".git").is_file());
    case.close().await;
}

#[tokio::test]
async fn a_master_turn_is_prepared_on_a_repository_whose_config_lists_more_than_64_kib() {
    use std::fmt::Write;
    let case = Fixture::onboarded().await;
    case.register().await;
    let config = case.repository.join(".git/config");
    let mut text = std::fs::read_to_string(&config).unwrap();
    for branch in 0..1000 {
        let name = format!("feature/scholar-scope-{branch:04}");
        write!(
            text,
            "[branch \"{name}\"]\n\tremote = origin\n\tmerge = refs/heads/{name}\n"
        )
        .unwrap();
    }
    std::fs::write(&config, &text).unwrap();
    assert!(text.len() > 100_000);
    let prepared = match case.runtime(case.prepare()).await {
        Ok(MasterRuntimeResult::Prepared(prepared)) => prepared,
        Ok(_) => panic!("a repository with many branches must prepare its master turn"),
        Err(failure) => panic!("many branches must not stop a master turn: {failure:?}"),
    };
    assert_eq!(prepared.repository, case.repository);
    assert_eq!(prepared.lease.state, LeaseState::Ready);
    case.close().await;
}

#[tokio::test]
async fn unregistered_local_rcli_is_configuration_required_without_admission_or_allocation() {
    let case = Fixture::new(false).await;
    let before = case.snapshot();
    let result = case.runtime(case.prepare()).await.unwrap();
    let MasterRuntimeResult::Turn(blocked) = result else {
        panic!("a master no pool key can run blocks its turn, as every master does");
    };
    assert_eq!(blocked.phase, MasterTurnPhase::Blocked);
    assert_eq!(blocked.reason, Some(MasterReason::RuntimeUnavailable));
    assert_eq!(
        blocked.cause.as_deref(),
        Some("rcli_profile_configuration_required")
    );
    assert_eq!(case.snapshot().counts, before.counts);
    assert_eq!(before.counts, [0; 6]);
    assert!(!case.home.join(".backboard/keys.json").exists());
    case.close().await;
}

#[tokio::test]
async fn engine_generated_local_rcli_home_registers_on_first_prepare_without_owner_path() {
    let case = Fixture::new(true).await;
    let connection = case
        .config
        .settings
        .model_access
        .as_ref()
        .unwrap()
        .connections[0]
        .clone();
    let profile_ref = super::super::account_profile::configured_profile_ref(&connection).unwrap();
    let homes = case.root.path().join("configured-sdk-homes");
    std::fs::create_dir(&homes).unwrap();
    std::fs::set_permissions(&homes, std::fs::Permissions::from_mode(0o700)).unwrap();
    let generated = homes.join(format!(
        "{:x}",
        Sha256::digest(profile_ref.as_str().as_bytes())
    ));
    std::fs::rename(&case.home, &generated).unwrap();
    let result = case.runtime(case.prepare()).await.unwrap();
    let MasterRuntimeResult::Prepared(prepared) = result else {
        panic!("an engine-generated SDK home must register and prepare the first conversation");
    };
    assert_eq!(prepared.text, PROMPT);
    assert_eq!(prepared.attempt.connection, connection);
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let record = registration(
        &tx,
        &case.session.session_id,
        &case.session.selection.target_profile_ref,
    )
    .unwrap()
    .unwrap();
    assert_eq!(
        record.proof.profile_ref,
        case.session.selection.target_profile_ref
    );
    drop(tx);
    case.close().await;
}

/// rv-sec3 G4 r2 F1: a master whose generated SDK home's config was written over and
/// put back outside an account save, the same bytes and inode with its modified time
/// reset, moves only its change time. The master's next turn is refused with
/// rcli_profile_changed_in_conversation (rv-code4 B22 F3) and its slot keeps its
/// generation; at RV-DS241-50 r1 the turn re-pinned it unseen. An undone save's put-back
/// still re-pins, in custody's rows.
#[tokio::test]
async fn a_master_turn_after_its_config_was_written_and_restored_outside_a_save_is_refused() {
    let case = Fixture::new(true).await;
    let config = generated_master_config(&case);
    let MasterRuntimeResult::Prepared(first) = case.runtime(case.prepare()).await.unwrap() else {
        panic!("an engine-generated SDK home must register and prepare the first turn");
    };
    fail_first(&case, &first, MasterReason::RuntimeUnavailable).await;
    write_and_restore_config(&config);

    let prepare = next_turn(&case, "after-write-and-restore").await;
    let refused = refused_cause(&case, prepare, "a write-and-restore").await;

    assert_eq!(
        refused.as_deref(),
        Some("rcli_profile_changed_in_conversation")
    );
    assert_master_generation(&case, 1);
    case.close().await;
}

/// rv-code4 B22 F2: the G4 refusal holds across a rebinding. A master writes over its
/// generated SDK home's config and puts it back, then the owner adds an account outside
/// the master's pool and sends, so the session moves to the new settings binding and
/// its registration reads stale. The turn is still refused, with
/// rcli_profile_changed_in_conversation, and the slot keeps its generation; before, the
/// stale registration took the owner's permissive register and re-pinned it unseen.
#[tokio::test]
async fn a_master_turn_after_a_write_and_restore_and_a_settings_rebind_is_refused() {
    let case = Fixture::kept().await;
    let config = generated_master_config(&case);
    let MasterRuntimeResult::Prepared(first) = case.runtime(case.prepare()).await.unwrap() else {
        panic!("an engine-generated SDK home must register and prepare the first turn");
    };
    fail_first(&case, &first, MasterReason::RuntimeUnavailable).await;
    write_and_restore_config(&config);
    let before = add_outside_account(&case).await;

    let prepare = next_turn(&case, "after-write-and-restore-and-rebind").await;
    let refused = refused_cause(&case, prepare, "a write-and-restore and a rebind").await;

    let (session, _) = case.view().await;
    assert_ne!(
        session.binding().0,
        &before,
        "the outside edit rebound the session"
    );
    assert_eq!(
        refused.as_deref(),
        Some("rcli_profile_changed_in_conversation")
    );
    assert_master_generation(&case, 1);
    case.close().await;
}

/// rv-code4 B22 R2-F1: after a rebinding, a real change to the chosen member's pinned
/// home, its config replaced by a copy (a new inode), is still named as a changed file.
/// The owner adds an account outside the master's pool and sends; the turn is refused
/// with rcli_profile_identity_changed and the slot keeps its generation. At d6bc5cf16
/// the pinned route's refusal was dropped and the turn named
/// master_configured_receipt_stale, a sign-in gap that fails over.
#[tokio::test]
async fn a_master_turn_after_a_replaced_config_and_a_settings_rebind_names_the_changed_file() {
    let case = Fixture::kept().await;
    let config = generated_master_config(&case);
    let MasterRuntimeResult::Prepared(first) = case.runtime(case.prepare()).await.unwrap() else {
        panic!("an engine-generated SDK home must register and prepare the first turn");
    };
    fail_first(&case, &first, MasterReason::RuntimeUnavailable).await;
    replace_config(&config);
    let before = add_outside_account(&case).await;

    let prepare = next_turn(&case, "after-replace-and-rebind").await;
    let refused = refused_cause(&case, prepare, "a replaced config and a rebind").await;

    let (session, _) = case.view().await;
    assert_ne!(
        session.binding().0,
        &before,
        "the outside edit rebound the session"
    );
    assert_eq!(refused.as_deref(), Some("rcli_profile_identity_changed"));
    assert_master_generation(&case, 1);
    case.close().await;
}

/// RV-DS241-50 r1: turn preparation re-pins a moved member only on the home its slot
/// pins. An owner-registered home whose config was replaced stays refused even when
/// the account's engine-generated home exists, and the slot is never moved onto it.
#[tokio::test]
async fn a_replaced_config_in_an_owner_home_is_never_moved_onto_the_generated_home() {
    let case = Fixture::new(true).await;
    let registered = case.register().await;
    let connection = case
        .config
        .settings
        .model_access
        .as_ref()
        .unwrap()
        .connections[0]
        .clone();
    let profile_ref = super::super::account_profile::configured_profile_ref(&connection).unwrap();
    let homes = case.root.path().join("configured-sdk-homes");
    std::fs::create_dir(&homes).unwrap();
    std::fs::set_permissions(&homes, std::fs::Permissions::from_mode(0o700)).unwrap();
    let generated = homes.join(format!(
        "{:x}",
        Sha256::digest(profile_ref.as_str().as_bytes())
    ));
    let config = case.home.join(".backboard/config.json");
    let bytes = std::fs::read(&config).unwrap();
    for folder in [generated.clone(), generated.join(".backboard")] {
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    private_file(&generated.join(".backboard/config.json"), &bytes);
    let MasterRuntimeResult::Prepared(first) = case.runtime(case.prepare()).await.unwrap() else {
        panic!("the registered owner home must prepare the first turn");
    };
    fail_first(&case, &first, MasterReason::RuntimeUnavailable).await;
    let replacement = case.home.join(".backboard/config.json.new");
    private_file(&replacement, &bytes);
    std::fs::rename(&replacement, &config).unwrap();

    let prepare = next_turn(&case, "after-replaced-config").await;
    if let Ok(MasterRuntimeResult::Prepared(_)) = case.runtime(prepare).await {
        panic!("a replaced owner config must not prepare on another home");
    }

    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let record = registration(
        &tx,
        &case.session.session_id,
        &case.session.selection.target_profile_ref,
    )
    .unwrap()
    .unwrap();
    assert_eq!(record.proof.registration_id, registered.registration_id);
    assert_eq!(record.proof.generation, registered.generation);
    drop(tx);
    case.close().await;
}

/// KY-M1 (DEC-150 addendum 9a): a keyed RCLI key saved under an rcli builtin provider id
/// before the fix, already in the pool, left a slot-less generated home naming that id.
/// The next master turn rewrites it to the hashed id, registers it and prepares, with no
/// owner action.
#[tokio::test]
async fn a_pooled_keyed_rcli_key_saved_before_the_builtin_id_fix_prepares_the_master_unaided() {
    let case = Fixture::legacy_keyed().await;
    let connection = legacy_keyed_connection();
    assert_eq!(
        case.config
            .settings
            .model_access
            .as_ref()
            .unwrap()
            .connections[0],
        connection
    );
    let config = legacy_generated_home(&case, &connection, &connection.model.id);
    let result = case.runtime(case.prepare()).await.unwrap();
    let MasterRuntimeResult::Prepared(prepared) = result else {
        panic!("the legacy keyed home must be repaired and prepare the first turn");
    };
    assert_eq!(prepared.attempt.connection, connection);
    let hashed = crate::rcli_host::configured_sdk_connection(&connection)
        .unwrap()
        .provider;
    assert_ne!(hashed, connection.provider);
    let written: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(written["model"]["provider"], hashed.as_str());
    assert_eq!(written["providers"][0]["id"], hashed.as_str());
    case.close().await;
}

/// KY-M1: only the engine's exact old config for the key is repaired; a slot-less home
/// holding anything else still stops the master and is left as it was.
#[tokio::test]
async fn a_slotless_keyed_home_that_is_not_the_engine_s_old_config_still_stops_the_master() {
    let case = Fixture::legacy_keyed().await;
    let connection = legacy_keyed_connection();
    let config = legacy_generated_home(&case, &connection, "another-model");
    let before = std::fs::read(&config).unwrap();
    let Err(refused) = case.runtime(case.prepare()).await else {
        panic!("a slot-less home that is not the engine's old config must stop the master");
    };
    assert_eq!(refused.0, "rcli_profile_configuration_invalid");
    assert_eq!(std::fs::read(&config).unwrap(), before);
    case.close().await;
}

/// KY-M1: a master turn stopped because the engine could not write its keyed account's
/// saved key keeps the reason a changed profile gets, so pausing and failover stay as
/// they are, and carries the line naming each app's way to set the account up again.
#[tokio::test]
async fn a_master_turn_stopped_for_its_key_setup_names_each_way_to_set_it_up_again() {
    let case = Fixture::new(true).await;
    case.register().await;
    let MasterRuntimeResult::Prepared(prepared) = case.runtime(case.prepare()).await.unwrap()
    else {
        panic!("the registered key must prepare the first turn");
    };
    case.runtime(MasterRuntimeCommand::Observe {
        host: case.started.host.clone(),
        project_id: case.session.project_id.clone(),
        session_id: case.session.session_id.clone(),
        turn_id: case.turn.id.clone(),
        attempt_id: prepared.attempt.request.id.clone(),
        observation: MasterRuntimeObservation::Failed {
            reason: MasterReason::ConnectionChanged,
            cause: crate::master_session::RCLI_KEY_SETUP_CAUSE.into(),
        },
    })
    .await
    .unwrap();
    let (_, turns) = case.view().await;
    let stopped = turns.iter().find(|turn| turn.id == case.turn.id).unwrap();
    assert_eq!(
        (
            stopped.phase,
            stopped.reason,
            stopped.cause.as_deref(),
            stopped.detail.as_deref()
        ),
        (
            MasterTurnPhase::Failed,
            Some(MasterReason::ConnectionChanged),
            Some("rcli_master_key_setup_required"),
            Some(
                "The master could not set this account's API key up for agents. Set it up again: in the app, Settings, Pools, Get ready for agents; in the terminal app, Model access, k Private profile setup."
            )
        )
    );
    case.close().await;
}

#[tokio::test]
async fn configured_receipt_cannot_be_replaced_by_a_constructed_proof() {
    let case = Fixture::new(false).await;
    let proof = case.register().await;
    let before = case.snapshot();
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    validate_bootstrap_auth(
        &tx,
        &case.session,
        &MasterBootstrapAuth::RcliConfigured(proof.clone()),
    )
    .unwrap();
    for field in 0..11 {
        let mut changed = proof.clone();
        match field {
            0 => changed.registration_id = RequestId::new("unregistered-proof").unwrap(),
            1 => changed.generation = changed.generation.checked_add(1).unwrap(),
            2 => changed.engine_epoch = "0".repeat(64),
            3 => changed.config_id = ConfigId::new("0".repeat(64)).unwrap(),
            4 => changed.source_hash = "0".repeat(64),
            5 => changed.profile_ref = reference("unregistered-profile"),
            6 => changed.connection.account_ref = reference("foreign-account"),
            7 => changed.profile_config_sha256 = "0".repeat(64),
            8 => changed.database_instance = "0".repeat(64),
            9 => changed.custody_id = "unregistered-custody".into(),
            10 => changed.at_revision = Cursor::new(u64::MAX),
            _ => unreachable!(),
        }
        assert!(
            validate_bootstrap_auth(
                &tx,
                &case.session,
                &MasterBootstrapAuth::RcliConfigured(changed)
            )
            .is_err(),
            "constructed proof field {field} was trusted"
        );
    }
    drop(tx);
    drop(db);
    assert_eq!(case.snapshot(), before);
    assert_eq!(before.counts, [0; 6]);
    case.close().await;
}

#[tokio::test]
async fn configured_profiles_of_closed_sessions_leave_room_for_a_new_registration() {
    let case = Fixture::new(true).await;
    let project = case.session.project_id.clone();
    let mut session = case.session.session_id.clone();
    for cycle in 0..=crate::master_session::SESSIONS_MAX {
        let registered = case
            .runtime(MasterRuntimeCommand::RegisterRcliProfile {
                host: case.started.host.clone(),
                project_id: project.clone(),
                session_id: session.clone(),
                configured_home: case.home.clone(),
            })
            .await;
        match registered {
            Ok(MasterRuntimeResult::ConfiguredProfile(_)) => {}
            Ok(_) => panic!("register {cycle}: another runtime result"),
            Err(failure) => panic!("register {cycle}: {}", failure.0),
        }
        owner_apply(
            &case.storage,
            &case.started,
            &format!("close-configured-{cycle}"),
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Close {
                    project_id: project.clone(),
                    session_id: session.clone(),
                },
            },
        )
        .await;
        let basis = owner_read(
            &case.storage,
            &case.started,
            OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 1,
            },
        )
        .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("actual model source basis missing");
        };
        owner_apply(
            &case.storage,
            &case.started,
            &format!("open-configured-{cycle}"),
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Open {
                    project_id: project.clone(),
                    expected_config_id: basis.config_id,
                    expected_source_hash: source_hash,
                    selection: Box::new(case.session.selection.clone()),
                },
            },
        )
        .await;
        let state = owner_read(
            &case.storage,
            &case.started,
            OwnerRead::ProjectMaster {
                project_id: project.clone(),
            },
        )
        .await;
        let OwnerData::ProjectMaster {
            session: Some(opened),
        } = state.data
        else {
            panic!("actual reopened master session missing");
        };
        assert_ne!(opened.session_id, session);
        session = opened.session_id;
    }
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let registrations: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM configured_master_profiles",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(registrations >= 1);
    assert!(registrations <= CONFIGURED_PROFILES_MAX as i64);
    drop(tx);
    case.close().await;
}

#[tokio::test]
async fn configured_registration_does_not_bypass_current_settings_or_host_epoch() {
    let case = Fixture::new(false).await;
    case.register().await;
    let before = case.snapshot();
    let mut foreign = case.started.host.clone();
    foreign.engine_epoch = "0".repeat(64);
    let result = case
        .runtime(MasterRuntimeCommand::Prepare {
            host: foreign,
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
        })
        .await;
    assert_eq!(result.err().unwrap().0, "owner_runtime_stale");
    assert_eq!(case.snapshot(), before);
    private_file(
        &case.global,
        CONFIG
            .replace("timeout_ms = 2000", "timeout_ms = 2001")
            .as_bytes(),
    );
    let result = case.runtime(case.prepare()).await;
    assert_eq!(result.err().unwrap().0, "settings_source_conflict");
    assert_eq!(case.snapshot(), before);
    case.close().await;
}

#[tokio::test]
async fn stopping_observation_waits_for_a_pending_settings_publication() {
    let case = Fixture::new(true).await;
    case.register().await;
    let MasterRuntimeResult::Prepared(prepared) = case.runtime(case.prepare()).await.unwrap()
    else {
        panic!("registered local RCLI must prepare the actual turn");
    };
    let basis = owner_read(
        &case.storage,
        &case.started,
        OwnerRead::ModelAccessPage {
            collection: ModelAccessCollection::Accounts,
            cursor: None,
            limit: 1,
        },
    )
    .await;
    let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
        panic!("actual model source basis missing");
    };
    let db = Connection::open(&case.path).unwrap();
    db.execute_batch("CREATE TRIGGER publication_abort BEFORE UPDATE ON settings_publication BEGIN SELECT RAISE(ABORT, 'actual_journal_abort'); END;").unwrap();
    let (send, receive) = oneshot::channel();
    case.storage
        .sender
        .try_send(Job::OwnerApply(
            case.started.owners[0].admission.clone(),
            OwnerRequest {
                protocol_version: PROTOCOL_VERSION,
                request_id: RequestId::new("pending-publication").unwrap(),
                expected_revision: basis.revision,
                operation: OwnerOperation::Apply {
                    mutation: OwnerMutation::ModelAccess {
                        expected_config_id: basis.config_id,
                        expected_source_hash: source_hash,
                        edit: crate::config::ModelAccessEdit::AddAccount {
                            account_ref: reference("pending-account"),
                        },
                    },
                },
            },
            send,
        ))
        .unwrap_or_else(|_| panic!("actual owner mutation queue unavailable"));
    assert!(receive.await.unwrap().is_err());
    db.execute_batch("DROP TRIGGER publication_abort").unwrap();
    drop(db);
    let before = case.snapshot();
    let observed = case
        .runtime(MasterRuntimeCommand::Observe {
            host: case.started.host.clone(),
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
            turn_id: case.turn.id.clone(),
            attempt_id: prepared.attempt.request.id.clone(),
            observation: MasterRuntimeObservation::Disconnected,
        })
        .await;
    assert_eq!(observed.err().unwrap().0, "settings_publication_pending");
    assert_eq!(case.snapshot(), before);
    let path = case.path.clone();
    let global = case.global.clone();
    let root = case.close().await;
    let (storage, config) =
        Storage::open_with_settings_sources(&path, &global, &root.path().join("project.toml"))
            .await
            .unwrap();
    assert!(
        config
            .settings
            .model_access
            .as_ref()
            .unwrap()
            .accounts
            .contains(&reference("pending-account"))
    );
    let (send, receive) = oneshot::channel();
    storage
        .sender
        .try_send(Job::Close(send))
        .unwrap_or_else(|_| panic!("actual actor close queue unavailable"));
    receive.await.unwrap().unwrap();
}

#[tokio::test]
async fn configured_profile_capacity_counts_live_sessions_not_closed_history() {
    let case = Fixture::new(false).await;
    {
        let mut db = Connection::open(&case.path).unwrap();
        db.busy_timeout(std::time::Duration::from_secs(10)).unwrap();
        let tx = db.transaction().unwrap();
        for n in 0..CONFIGURED_PROFILES_MAX {
            tx.execute(
                "INSERT INTO configured_master_profiles(id,session,profile,body) VALUES (?1,?1,?1,'{}')",
                [format!("closed-{n:03}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
    }
    let proof = case.register().await;
    let db = case.db();
    let count = |sql: &str| -> i64 { db.query_row(sql, [], |row| row.get(0)).unwrap() };
    assert_eq!(
        count("SELECT COUNT(*) FROM configured_master_profiles"),
        CONFIGURED_PROFILES_MAX as i64
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM configured_master_profiles WHERE id='closed-000'"),
        0
    );
    let stored: String = db
        .query_row(
            "SELECT id FROM configured_master_profiles WHERE session=?1",
            [case.session.session_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, proof.registration_id.as_str());
    drop(db);
    case.close().await;
}

#[tokio::test]
async fn a_queued_turn_holds_the_session_until_its_own_cancel_frees_it() {
    let case = Fixture::new(false).await;
    let rejected = |code: &str| OwnerResult::Rejected { code: code.into() };
    let before = case.snapshot();
    assert_eq!(
        case.result(case.submit("second-turn", "Still outstanding."))
            .await,
        rejected("master_turn_outstanding")
    );
    let cancel = |id: &str, turn: &str| {
        case.action(
            id,
            MasterOwnerAction::Cancel {
                project_id: case.session.project_id.clone(),
                session_id: case.session.session_id.clone(),
                turn_id: RequestId::new(turn).unwrap(),
            },
        )
    };
    assert_eq!(
        case.result(cancel("cancel-other", "another-turn")).await,
        rejected("master_turn_not_current")
    );
    let held = case.snapshot();
    assert_eq!((held.turn, held.counts), (before.turn, before.counts));
    assert_eq!(
        case.result(cancel("cancel-turn", case.turn.id.as_str()))
            .await,
        OwnerResult::Applied
    );
    let (session, turns) = case.view().await;
    assert_eq!(session.current_turn, None);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].id, case.turn.id);
    assert_eq!(turns[0].phase, MasterTurnPhase::Cancelled);
    assert_eq!(turns[0].attempt, None);
    assert_eq!(turns[0].runtime, None);
    assert_eq!(turns[0].reply, None);
    assert_eq!(
        case.result(case.submit("next-turn", "Use the revised requirement."))
            .await,
        OwnerResult::Applied
    );
    let (session, turns) = case.view().await;
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1].ordinal, 2);
    assert_eq!(turns[1].phase, MasterTurnPhase::Queued);
    assert_eq!(session.current_turn, Some(turns[1].id.clone()));
    case.close().await;
}

#[tokio::test]
async fn closing_a_session_cancels_its_queued_turn_and_refuses_late_input() {
    let case = Fixture::new(false).await;
    let close = case.action(
        "close-session",
        MasterOwnerAction::Close {
            project_id: case.session.project_id.clone(),
            session_id: case.session.session_id.clone(),
        },
    );
    assert_eq!(case.result(close).await, OwnerResult::Applied);
    let (session, turns) = case.view().await;
    assert_eq!(session.phase, MasterSessionPhase::Parked);
    assert_eq!(session.current_turn, None);
    assert_eq!(turns.len(), 1);
    assert_eq!(turns[0].phase, MasterTurnPhase::Cancelled);
    assert_eq!(turns[0].attempt, None);
    assert_eq!(turns[0].runtime, None);
    assert_eq!(turns[0].reply, None);
    assert_eq!(
        case.result(case.submit("late-turn", "Must not reopen."))
            .await,
        OwnerResult::Rejected {
            code: "master_session_not_active".into()
        }
    );
    assert_eq!(case.view().await.1, turns);
    case.close().await;
}

#[tokio::test]
async fn removing_the_project_ends_its_open_master_conversation_in_the_same_action() {
    let case = Fixture::new(false).await;
    let project = case.session.project_id.clone();
    let remove = (
        "remove-project".to_owned(),
        OwnerMutation::RemoveProject {
            project_id: project.clone(),
        },
    );
    assert_eq!(case.result(remove).await, OwnerResult::Applied);
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let revision = owner_revision(&tx).unwrap();
    let session = load_session(&tx, &case.session.session_id, revision)
        .unwrap()
        .unwrap();
    assert_eq!(session.phase, MasterSessionPhase::Closed);
    assert_eq!(session.current_turn, None);
    assert_eq!(
        session.version.value(),
        revision,
        "the conversation ends in the removal itself, not in a later change"
    );
    let turn = load_turn(&tx, &session, &case.turn.id, revision)
        .unwrap()
        .unwrap();
    assert_eq!(turn.phase, MasterTurnPhase::Cancelled);
    let projects: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE id=?1",
            [project.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(projects, 0, "the project is removed in the same action");
    drop(tx);
    case.close().await;
}

impl Fixture {
    /// Starts a new session as the owner's New session does, parking the live one and
    /// cancelling its turn, then submits `message` there. Inputs: the owner operation
    /// name and the message. Output: the fixture bound to the new session and its turn.
    async fn renewed(self, id: &str, message: &str) -> Self {
        let basis = owner_read(
            &self.storage,
            &self.started,
            OwnerRead::ModelAccessPage {
                collection: ModelAccessCollection::Pools,
                cursor: None,
                limit: 1,
            },
        )
        .await;
        let OwnerData::ModelAccessPage { source_hash, .. } = basis.data else {
            panic!("actual model source basis missing");
        };
        owner_apply(
            &self.storage,
            &self.started,
            id,
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::NewSession {
                    project_id: self.session.project_id.clone(),
                    expected_config_id: basis.config_id,
                    expected_source_hash: source_hash,
                    selection: Box::new(self.session.selection.clone()),
                    cancel_running: true,
                },
            },
        )
        .await;
        self.submitted(&format!("{id}-submit"), message).await
    }

    /// Makes a parked session live again as the owner's Resume does, parking the live
    /// one and cancelling its turn, then submits `message` there. Inputs: the owner
    /// operation name, the parked session and the message. Output: the fixture bound to
    /// that session and its new turn.
    async fn resumed(self, id: &str, session: &SessionId, message: &str) -> Self {
        owner_apply(
            &self.storage,
            &self.started,
            id,
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Resume {
                    project_id: self.session.project_id.clone(),
                    session_id: session.clone(),
                    cancel_running: true,
                },
            },
        )
        .await;
        self.submitted(&format!("{id}-submit"), message).await
    }

    /// Submits `message` to the project's live session and binds the fixture to that
    /// session and the turn it queued. Inputs: the owner operation name and the message.
    /// Output: the rebound fixture.
    async fn submitted(mut self, id: &str, message: &str) -> Self {
        let project_id = self.session.project_id.clone();
        let live = |state: OwnerReadReply| match state.data {
            OwnerData::ProjectMaster {
                session: Some(session),
            } => *session,
            _ => panic!("actual live master session missing"),
        };
        let session = live(
            owner_read(
                &self.storage,
                &self.started,
                OwnerRead::ProjectMaster {
                    project_id: project_id.clone(),
                },
            )
            .await,
        );
        owner_apply(
            &self.storage,
            &self.started,
            id,
            OwnerMutation::ProjectMaster {
                action: MasterOwnerAction::Submit {
                    project_id: project_id.clone(),
                    session_id: session.session_id.clone(),
                    text: message.into(),
                },
            },
        )
        .await;
        let session = live(
            owner_read(
                &self.storage,
                &self.started,
                OwnerRead::ProjectMaster {
                    project_id: project_id.clone(),
                },
            )
            .await,
        );
        let state = owner_read(
            &self.storage,
            &self.started,
            OwnerRead::ProjectMasterTurns {
                project_id,
                session_id: session.session_id.clone(),
                cursor: None,
                limit: 8,
            },
        )
        .await;
        let OwnerData::ProjectMasterTurns { page } = state.data else {
            panic!("actual master turns missing");
        };
        let queued: Vec<MasterTurnView> = page
            .turns
            .into_iter()
            .filter(|turn| turn.phase == MasterTurnPhase::Queued)
            .collect();
        assert_eq!(queued.len(), 1, "one queued turn in the live session");
        self.session = session;
        self.turn = queued.into_iter().next().unwrap();
        assert_eq!(self.session.current_turn.as_ref(), Some(&self.turn.id));
        self
    }
}

const RENEWED: &str = "Name the main language of this project in one word.";

/// A session the owner started with New session prepares its turns as an opened one
/// does: its New session provenance is the request that created it, so the master
/// bootstrap admits it instead of refusing every turn with a scope conflict.
#[tokio::test]
async fn a_session_started_with_new_session_prepares_its_turns() {
    let first = Fixture::new(true).await;
    let opened = first.session.session_id.clone();
    let case = first.renewed("renew-rcli-master", RENEWED).await;
    assert_ne!(case.session.session_id, opened);
    let proof = case.register().await;
    let prepared = match case.runtime(case.prepare()).await {
        Ok(MasterRuntimeResult::Prepared(prepared)) => prepared,
        Ok(_) => panic!("a renewed session must prepare its turn"),
        Err(failure) => panic!("a renewed session must prepare its turn: {}", failure.0),
    };
    assert_eq!(prepared.text, RENEWED);
    assert_eq!(prepared.session.session_id, case.session.session_id);
    assert_eq!(
        prepared.attempt.request.task_id,
        turn_task_id(&case.session.session_id, &case.turn.id).unwrap()
    );
    assert_eq!(prepared.attempt.connection, proof.connection);
    assert_eq!(prepared.lease.state, LeaseState::Ready);
    assert_eq!(prepared.lease.owner_session, case.session.session_id);
    assert_eq!(prepared.lease.task_id, case.session.bootstrap_task_id);
    assert!(prepared.cwd.is_dir());
    case.close().await;
}

/// A renewed session the owner parks and resumes keeps preparing turns: Resume keeps
/// the New session provenance the session was created with, and the bootstrap admits it.
#[tokio::test]
async fn a_resumed_renewed_session_prepares_its_turns() {
    let case = Fixture::new(true)
        .await
        .renewed("renew-rcli-master", RENEWED)
        .await;
    let renewed = case.session.session_id.clone();
    let case = case
        .renewed("renew-rcli-master-again", PROMPT)
        .await
        .resumed("resume-renewed-master", &renewed, RENEWED)
        .await;
    assert_eq!(case.session.session_id, renewed);
    assert_eq!(case.session.phase, MasterSessionPhase::Configured);
    let proof = case.register().await;
    let prepared = match case.runtime(case.prepare()).await {
        Ok(MasterRuntimeResult::Prepared(prepared)) => prepared,
        Ok(_) => panic!("a resumed renewed session must prepare its turn"),
        Err(failure) => panic!(
            "a resumed renewed session must prepare its turn: {}",
            failure.0
        ),
    };
    assert_eq!(prepared.text, RENEWED);
    assert_eq!(prepared.session.session_id, renewed);
    assert_eq!(
        prepared.attempt.request.task_id,
        turn_task_id(&renewed, &case.turn.id).unwrap()
    );
    assert_eq!(prepared.attempt.connection, proof.connection);
    assert_eq!(prepared.lease.state, LeaseState::Ready);
    assert_eq!(prepared.lease.owner_session, renewed);
    case.close().await;
}

impl Fixture {
    /// Stores a team plan for the fixture's project: a tools-on Claude Code master and
    /// one tools-on Codex sub-agent. Output: the stored setup.
    fn team_plan(&self) -> crate::project_setup::StoredSetup {
        let setup = crate::project_setup::StoredSetup {
            project_id: self.session.project_id.clone(),
            name: "Tools".into(),
            about: String::new(),
            tech_stack: vec![],
            platforms: vec![],
            research: vec![],
            pool: None,
            team: crate::project_setup::TeamPlan {
                master: "claude-code".into(),
                master_tools: true,
                sub_harnesses: vec![crate::project_setup::HarnessSlot {
                    harness: "codex".into(),
                    agents: 1,
                    tools: true,
                }],
                min_agents: 1,
                max_agents: 2,
            },
            documents: vec![],
            folder: self.repository.display().to_string(),
            created_ms: 1,
            revision: 1,
        };
        let writer = Connection::open(&self.path).unwrap();
        writer
            .busy_timeout(std::time::Duration::from_secs(10))
            .unwrap();
        writer
            .execute(
                "INSERT OR REPLACE INTO project_setups VALUES (?1,?2)",
                params![
                    setup.project_id.as_str(),
                    serde_json::to_string(&setup).unwrap()
                ],
            )
            .unwrap();
        setup
    }

    /// Runs the fixture's turn to a reply of the given text that carries the given
    /// delegate calls and reports that reply `times` times, as a host resending its
    /// observation does. Inputs: the text, the calls and the count. Output: the reply.
    async fn reply_with(
        &self,
        text: &str,
        delegations: Vec<crate::team::Delegation>,
        times: usize,
    ) -> MasterReply {
        let MasterRuntimeResult::Prepared(prepared) = self.runtime(self.prepare()).await.unwrap()
        else {
            panic!("registered local RCLI must prepare the actual turn");
        };
        assert!(!prepared.tools, "an RCLI master runs without tools");
        let command = |observation| MasterRuntimeCommand::Observe {
            host: self.started.host.clone(),
            project_id: self.session.project_id.clone(),
            session_id: self.session.session_id.clone(),
            turn_id: self.turn.id.clone(),
            attempt_id: prepared.attempt.request.id.clone(),
            observation,
        };
        let MasterRuntimeResult::Profile(guard) = self
            .runtime(MasterRuntimeCommand::AcquireProfile {
                host: self.started.host.clone(),
                project_id: self.session.project_id.clone(),
                session_id: self.session.session_id.clone(),
                turn_id: self.turn.id.clone(),
                attempt_id: prepared.attempt.request.id.clone(),
            })
            .await
            .unwrap()
        else {
            panic!("actual configured RCLI Execution guard missing");
        };
        let runtime = MasterRuntimeView {
            runtime_ref: RequestId::new("tools-runtime").unwrap(),
            artifact_sha256: "a".repeat(64),
            native_session_id: Some("tools-native".into()),
        };
        self.runtime(command(MasterRuntimeObservation::Running {
            runtime: runtime.clone(),
        }))
        .await
        .unwrap();
        let reply = MasterReply {
            text: text.into(),
            provenance: crate::master_session::MasterReplyProvenance {
                attempt_id: prepared.attempt.request.id.clone(),
                runtime_ref: runtime.runtime_ref.clone(),
                capture_ref: RequestId::new("tools-capture").unwrap(),
                capture_sequence: 3,
            },
        };
        for _ in 0..times {
            let MasterRuntimeResult::Turn(turn) = self
                .runtime(command(MasterRuntimeObservation::Response {
                    reply: reply.clone(),
                    delegations: delegations.clone(),
                }))
                .await
                .unwrap()
            else {
                panic!("an observed reply returns the turn");
            };
            assert_eq!(turn.phase, MasterTurnPhase::Succeeded);
            assert_eq!(turn.reply.as_ref(), Some(&reply));
        }
        drop(guard);
        reply
    }
}

#[tokio::test]
async fn a_long_master_reply_is_stored_and_read_back_whole() {
    let case = Fixture::new(true).await;
    case.register().await;
    let text = "The README now covers setup, tests and the release steps.\n".repeat(190);
    assert!(text.len() > 8192);
    let reply = case.reply_with(&text, Vec::new(), 1).await;
    assert_eq!(reply.text, text);
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let revision = owner_revision(&tx).unwrap();
    let session = load_session(&tx, &case.session.session_id, revision)
        .unwrap()
        .unwrap();
    let turn = load_turn(&tx, &session, &case.turn.id, revision)
        .unwrap()
        .unwrap();
    assert_eq!(turn.phase, MasterTurnPhase::Succeeded);
    assert_eq!(turn.reply.map(|reply| reply.text), Some(text));
    drop(tx);
    case.close().await;
}

#[tokio::test]
async fn a_master_reply_hands_its_delegate_calls_to_the_team_once() {
    let case = Fixture::new(true).await;
    case.register().await;
    let setup = case.team_plan();
    let delegations = vec![
        crate::team::Delegation {
            harness: None,
            prompt: "Write the README".into(),
            role: None,
        },
        crate::team::Delegation {
            harness: Some("claude-code".into()),
            prompt: "Check the tests".into(),
            role: None,
        },
    ];
    case.reply_with("I handed the README to a sub-agent.", delegations, 2)
        .await;
    let db = case.db();
    let tasks: Vec<(String, String, String)> = db
        .prepare("SELECT source,harness,prompt FROM team_tasks WHERE project=?1 ORDER BY id")
        .unwrap()
        .query_map([setup.project_id.as_str()], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        tasks,
        [(
            "owner".to_owned(),
            "codex".to_owned(),
            "Write the README".to_owned()
        )]
    );
    let notes: Vec<String> = db
        .prepare("SELECT text FROM team_messages WHERE project=?1 AND author='engine'")
        .unwrap()
        .query_map([setup.project_id.as_str()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        notes,
        [
            "The conversation master's tasks were not handed off:\n- Check the tests: that harness is not in the team plan"
        ]
    );
    drop(db);
    case.close().await;
}

#[tokio::test]
async fn a_conversation_task_that_ends_comes_back_to_the_conversation_and_the_next_turn() {
    use crate::master_session::MasterTask;
    let case = Fixture::new(true).await;
    case.register().await;
    let setup = case.team_plan();
    case.reply_with(
        "I handed the README to a sub-agent.",
        vec![crate::team::Delegation {
            harness: None,
            prompt: "Write the README".into(),
            role: None,
        }],
        1,
    )
    .await;
    let task: i64 = case
        .db()
        .query_row(
            "SELECT id FROM team_tasks WHERE project=?1",
            [setup.project_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    let id = u64::try_from(task).unwrap();
    let (_, turns) = case.view().await;
    assert_eq!(
        turns[0].tasks,
        [MasterTask {
            task: id,
            note: None
        }],
        "the conversation shows the task it handed off while it waits"
    );
    let writer = Connection::open(&case.path).unwrap();
    writer
        .busy_timeout(std::time::Duration::from_secs(10))
        .unwrap();
    let tx = writer.unchecked_transaction().unwrap();
    assert_eq!(
        crate::team::execute(
            &tx,
            &crate::config::ModelAccess::default(),
            &crate::team::TeamAction::Cancel {
                project_id: setup.project_id.clone(),
                task: Some(task),
            },
        )
        .unwrap(),
        crate::protocol::Outcome::Applied
    );
    tx.commit().unwrap();
    drop(writer);
    let mut team = crate::team::runtime::TeamRuntime::open(
        &case.path,
        &case.root.path().join("team"),
        BTreeMap::new(),
        Box::new(|| None),
        crate::team::runtime::TeamLimits::default(),
    )
    .unwrap();
    team.tick().unwrap();
    team.shutdown().unwrap();
    let note = format!(
        "Task {task} \"Write the README\" (codex) was stopped: Cancelled by you. Details in the Team view."
    );
    let (_, turns) = case.view().await;
    assert_eq!(
        turns[0].tasks,
        [MasterTask {
            task: id,
            note: Some(note.clone())
        }],
        "the engine's note on the ended task shows under the turn that handed it off"
    );
    assert_eq!(
        case.result(case.submit("next-turn", "What did the sub-agent do?"))
            .await,
        OwnerResult::Applied
    );
    let (_, turns) = case.view().await;
    let mut db = case.db();
    let tx = db.transaction().unwrap();
    let text = prompt(&tx, &case.session, &turns[1]).unwrap();
    drop(tx);
    assert!(
        text.contains(&format!(
            "You: I handed the README to a sub-agent.\nEngine: {note}"
        )),
        "the next turn's context carries the note: {text}"
    );
    case.close().await;
}
