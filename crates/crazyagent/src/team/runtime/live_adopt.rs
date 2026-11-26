use super::*;

impl TeamRuntime {
    /// Binds a live master that just started to the session it runs, with the seed its
    /// briefing carries: the earlier turns for a fresh conversation, or the turns a kept
    /// conversation does not hold for one that continues. A master that did not start live
    /// and unbriefed is stopped and its start refused, so a start that read the switch off
    /// never ends the team thread (RV-DS241-121). Takes the agent, the session, the seed and
    /// the note of the start. Returns the agent and the note, or the refusal.
    pub(super) fn seat_session(
        &mut self,
        id: i64,
        session: Option<&SessionId>,
        seed: Option<String>,
        note: Option<String>,
    ) -> Start<(i64, Option<String>)> {
        let Some(worker) = self
            .workers
            .get_mut(&id)
            .filter(|worker| worker.live && !worker.briefed)
        else {
            if let Some(worker) = self.workers.get_mut(&id) {
                worker.stop_for("engine_stop:not_live");
                let _ = worker.orders.send(Order::Stop);
            }
            return Start::Refused(MASTER_NOT_LIVE.to_owned());
        };
        worker.session = session.map(|session| session.as_str().to_owned());
        worker.seed = seed;
        Start::Started((id, note))
    }

    /// Seats a live master the Live terminal switch started before any master session in
    /// the project's live session when that session is still empty: no owner turn and no
    /// kept conversation. The reads and the seat run in one immediate transaction, and the
    /// conversation the master named is kept for the session, so a restart resumes it.
    /// Takes the project, the master agent and the live switch. Returns any SQLite failure.
    pub(super) fn adopt_session(
        &mut self,
        project: &ProjectId,
        id: i64,
        on: bool,
    ) -> Result<(), EngineError> {
        let worker = &self.workers[&id];
        assert!(&worker.project == project && worker.role == AgentRole::Master);
        if !on || !worker.live || worker.stopping || worker.session.is_some() {
            return Ok(());
        }
        let tx = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(error)?;
        let Some(session) = crate::master_sessions::live_session(&tx, project)? else {
            return Ok(());
        };
        let id_of = SessionId::new(&session).map_err(error)?;
        if crate::master_sessions::has_owner_turn(&tx, &id_of)?
            || super::master::session_conversation(&tx, project, &session)?.is_some()
        {
            return Ok(());
        }
        if let Some(resume) = self.named.get(&id).cloned() {
            let kept = super::master::SessionConversation {
                agent: id,
                harness: launch_harness(&worker.harness)
                    .unwrap_or(&worker.harness)
                    .to_owned(),
                connection: worker.connection.as_str().to_owned(),
                resume,
            };
            super::master::keep_conversation(&tx, project, &session, &kept)?;
        }
        tx.commit().map_err(error)?;
        let worker = self.workers.get_mut(&id).expect("adopting master exists");
        worker.session = Some(session);
        assert!(worker.session.is_some() && !worker.stopping);
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "live_adopt/tests.rs"]
mod tests;
