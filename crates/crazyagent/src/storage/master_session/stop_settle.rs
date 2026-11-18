use super::*;

pub(super) fn settle_stopped_commit(
    db: &mut Connection,
    config: &EffectiveConfig,
    host: &HostStamp,
    binding: &MasterStopBinding,
) -> Result<MasterTurnView, EngineError> {
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(error)?;
    check_owner_host(&tx, host)?;
    let provenance = &binding.provenance;
    if binding.db_instance != host.db_instance
        || provenance.origin != "native_owner"
        || provenance.engine_epoch != host.engine_epoch
        || provenance.operation_id != binding.turn_id
        || provenance.revision_after != binding.input_cursor
        || provenance.event_cursor.as_ref() != Some(&binding.input_cursor)
        || provenance.revision_before.value().checked_add(1) != Some(binding.input_cursor.value())
        || provenance.admitted_epoch < 1
        || provenance.resulting_epoch != provenance.admitted_epoch
    {
        return Err(error("master_stop_binding_mismatch"));
    }
    let (stored, result) = owner_lookup(
        &tx,
        "native_owner",
        &provenance.owner_id,
        &provenance.operation_id,
        Some(&provenance.request_hash),
    )?
    .ok_or_else(|| error("master_stop_binding_mismatch"))?;
    if stored != *provenance
        || serde_json::from_slice::<OwnerResult>(&result).map_err(error)? != OwnerResult::Applied
    {
        return Err(error("master_stop_binding_mismatch"));
    }
    let (mut session, mut turn) = load_runtime_turn(
        &tx,
        host,
        &binding.project_id,
        &binding.session_id,
        &binding.turn_id,
    )?;
    if turn.input_cursor != binding.input_cursor {
        return Err(error("master_stop_binding_mismatch"));
    }
    if turn.phase.outstanding() && session.current_turn.as_ref() != Some(&turn.id) {
        return Err(error("master_turn_not_current"));
    }
    match turn.phase {
        MasterTurnPhase::Queued | MasterTurnPhase::Prepared => {
            turn = cancel_turn(&tx, turn)?;
        }
        MasterTurnPhase::Delivering | MasterTurnPhase::Running => {
            complete_local(&tx, &mut turn, MasterTurnPhase::Uncertain)?;
            turn.phase = MasterTurnPhase::Uncertain;
            turn.reason = Some(MasterReason::CaptureIncomplete);
        }
        MasterTurnPhase::Blocked
        | MasterTurnPhase::Succeeded
        | MasterTurnPhase::Failed
        | MasterTurnPhase::Cancelled
        | MasterTurnPhase::Uncertain => {
            tx.commit().map_err(error)?;
            return Ok(turn);
        }
        MasterTurnPhase::WaitingApproval => {
            return Err(error("master_runtime_state_unqualified"));
        }
    }
    persist_runtime(&tx, config, &mut session, &turn)?;
    tx.commit().map_err(error)?;
    Ok(turn)
}
