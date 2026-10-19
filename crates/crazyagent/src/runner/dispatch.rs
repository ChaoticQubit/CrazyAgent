use super::{
    AdmissionCheckOffer, Arc, Command, ControllerEpoch, DeniedReceipt, EPOCHS_MAX, FenceOrder,
    FenceReceipt, Mutex, OPERATIONS_MAX, Record, Reply, Request, RequestId, RunnerReady, State,
    TIMEOUT, invalid,
};
use crazyagent::engine::owner::RunnerClient;
use crazyagent::worker_dispatch::{
    DISPATCH_GENERATIONS_MAX, DISPATCH_RECEIPT_BYTES_MAX, DISPATCH_VIEW_BYTES_MAX, DeniedReason,
    FenceOutcome,
};
use std::io;

fn error(code: &'static str) -> Reply {
    Reply::Error { code }
}

fn receipt_matches(offer: &AdmissionCheckOffer, receipt: &DeniedReceipt) -> bool {
    receipt.dispatch_id == offer.dispatch_id
        && receipt.attempt == offer.attempt
        && receipt.delivery_generation == offer.delivery_generation
        && receipt.database_instance == offer.database_instance
        && receipt.offer_engine_epoch == offer.engine_epoch
        && receipt.registration_id == offer.registration_id
        && receipt.runner_boot_id == offer.runner_boot_id
        && receipt.offer_digest == offer.offer_digest
        && receipt.reason == DeniedReason::NativeRuntimeAdmissionUnavailable
        && receipt.native_identity.is_none()
}

impl State {
    pub(super) fn operation_count(&self) -> usize {
        self.operations.len() + self.denials.len()
    }

    fn latest_generation(&self, dispatch_id: &RequestId) -> Option<u64> {
        self.denials
            .keys()
            .chain(self.fences.keys())
            .filter_map(|(id, generation)| (id == dispatch_id).then_some(*generation))
            .max()
    }

    fn next_generation(&self, dispatch_id: &RequestId, generation: u64) -> bool {
        if !(1..=DISPATCH_GENERATIONS_MAX).contains(&generation) {
            return false;
        }
        match self.latest_generation(dispatch_id) {
            None => generation == 1,
            Some(previous) => {
                generation == previous + 1
                    && self
                        .fences
                        .get(&(dispatch_id.clone(), previous))
                        .is_some_and(|(_, receipt)| {
                            matches!(receipt.outcome, FenceOutcome::NotRecordedAtBarrier)
                        })
            }
        }
    }

    pub(super) fn dispatch_invariants(&self) {
        assert!(self.fences.len() + self.epochs.len() <= EPOCHS_MAX);
        for ((id, generation), (offer, receipt)) in &self.denials {
            assert_eq!(id, &offer.dispatch_id);
            assert_eq!(*generation, offer.delivery_generation);
            assert!((1..=DISPATCH_GENERATIONS_MAX).contains(generation));
            assert!(receipt_matches(offer, receipt));
            assert!(receipt.controller_generation > 0 && receipt.journal_sequence > 0);
        }
        for ((id, generation), (order, receipt)) in &self.fences {
            assert_eq!(id, &order.dispatch_id);
            assert_eq!(*generation, order.delivery_generation);
            assert_eq!(receipt.dispatch_id, order.dispatch_id);
            assert_eq!(receipt.delivery_generation, order.delivery_generation);
            assert_eq!(receipt.registration_id, order.registration_id);
            assert_eq!(receipt.runner_boot_id, order.runner_boot_id);
            assert!(receipt.journal_sequence > 0);
            match &receipt.outcome {
                FenceOutcome::Denied { receipt: denial } => {
                    let (offer, retained) = self.denials.get(&(id.clone(), *generation)).unwrap();
                    assert_eq!(denial.as_ref(), retained);
                    assert_eq!(order.offer_digest.as_ref(), Some(&offer.offer_digest));
                    assert!(denial.journal_sequence < receipt.journal_sequence);
                }
                FenceOutcome::NotRecordedAtBarrier => {
                    assert!(!self.denials.contains_key(&(id.clone(), *generation)));
                }
            }
        }
    }

    fn journal_available(&mut self) -> Result<(), &'static str> {
        if self.failed || self.journal.verify().is_err() {
            self.failed = true;
            return Err("journal_unavailable");
        }
        Ok(())
    }

    fn append_dispatch(&mut self, record: &Record) -> Result<(), &'static str> {
        if self.journal.append(record).is_err() {
            self.failed = true;
            return Err("journal_unavailable");
        }
        Ok(())
    }

    pub(super) fn register_engine(
        &mut self,
        registration_id: &RequestId,
        ready: RunnerReady,
    ) -> Reply {
        if let Err(code) = self.journal_available() {
            return error(code);
        }
        if ready.boot != self.identity
            || &ready.registration_id != registration_id
            || self.engine_registration.as_ref().is_some_and(|original| {
                original.registration_id != ready.registration_id
                    || original.attempt != ready.attempt
                    || original.database_instance != ready.database_instance
            })
        {
            return error("engine_registration_conflict");
        }
        if ControllerEpoch::new(ready.engine_epoch.clone()).is_err() {
            return error("engine_registration_conflict");
        }
        self.engine_registration = Some(ready.clone());
        self.assert_consistent();
        Reply::EngineReady { identity: ready }
    }

    fn validate_denial(
        &self,
        offer: &AdmissionCheckOffer,
        receipt: &DeniedReceipt,
        sequence: u64,
    ) -> Result<(), &'static str> {
        if offer.validate().is_err()
            || receipt.validate().is_err()
            || !receipt_matches(offer, receipt)
            || offer.runner_boot_id != self.identity.runner_id
            || self.controller.as_ref().map(ControllerEpoch::as_str)
                != Some(offer.engine_epoch.as_str())
            || receipt.controller_generation != self.generation
            || receipt.journal_sequence != sequence
            || !self.next_generation(&offer.dispatch_id, offer.delivery_generation)
            || self.operation_count() >= OPERATIONS_MAX
        {
            return Err("dispatch_denial_invalid");
        }
        Ok(())
    }

    pub(super) fn restore_denied(
        &mut self,
        offer: AdmissionCheckOffer,
        receipt: DeniedReceipt,
        sequence: u64,
    ) -> io::Result<()> {
        self.validate_denial(&offer, &receipt, sequence)
            .map_err(invalid)?;
        let key = (offer.dispatch_id.clone(), offer.delivery_generation);
        let previous = self.denials.insert(key, (offer, receipt));
        assert!(previous.is_none());
        Ok(())
    }

    fn consume_offer(
        &mut self,
        ready: &RunnerReady,
        offer: AdmissionCheckOffer,
    ) -> Result<DeniedReceipt, &'static str> {
        self.journal_available()?;
        if offer.validate().is_err()
            || offer.registration_id != ready.registration_id
            || offer.attempt != ready.attempt
            || offer.runner_boot_id != ready.boot.runner_id
            || offer.database_instance != ready.database_instance
            || offer.engine_epoch != ready.engine_epoch
            || self.engine_registration.as_ref() != Some(ready)
        {
            return Err("dispatch_offer_mismatch");
        }
        let key = (offer.dispatch_id.clone(), offer.delivery_generation);
        if let Some((original, receipt)) = self.denials.get(&key) {
            return if original == &offer {
                Ok(receipt.clone())
            } else {
                Err("dispatch_offer_conflict")
            };
        }
        if self.fences.contains_key(&key) {
            return Err("dispatch_generation_retired");
        }
        if self.operation_count() == OPERATIONS_MAX {
            return Err("operation_capacity");
        }
        if !self.next_generation(&offer.dispatch_id, offer.delivery_generation) {
            return Err("dispatch_generation_invalid");
        }
        let epoch = ControllerEpoch::new(offer.engine_epoch.clone())
            .map_err(|_| "dispatch_offer_mismatch")?;
        if self.controller.as_ref() != Some(&epoch) {
            let claimed = self
                .claim(self.identity.runner_id.clone(), self.generation, epoch)
                .map_err(|_| {
                    self.failed = true;
                    "journal_unavailable"
                })?;
            if let Reply::Error { code } = claimed {
                return Err(code);
            }
            assert!(matches!(claimed, Reply::Claimed { .. }));
        }
        let receipt = DeniedReceipt {
            dispatch_id: offer.dispatch_id.clone(),
            attempt: offer.attempt.clone(),
            delivery_generation: offer.delivery_generation,
            database_instance: offer.database_instance.clone(),
            offer_engine_epoch: offer.engine_epoch.clone(),
            registration_id: offer.registration_id.clone(),
            runner_boot_id: offer.runner_boot_id.clone(),
            controller_generation: self.generation,
            offer_digest: offer.offer_digest.clone(),
            journal_sequence: self.journal.sequence + 1,
            reason: DeniedReason::NativeRuntimeAdmissionUnavailable,
            native_identity: None,
        };
        self.validate_denial(&offer, &receipt, receipt.journal_sequence)?;
        self.append_dispatch(&Record::DispatchDenied {
            offer: Box::new(offer.clone()),
            receipt: Box::new(receipt.clone()),
        })?;
        let previous = self.denials.insert(key, (offer, receipt.clone()));
        assert!(previous.is_none());
        self.assert_consistent();
        assert_eq!(receipt.journal_sequence, self.journal.sequence);
        Ok(receipt)
    }

    fn fence_outcome(&self, order: &FenceOrder) -> Result<FenceOutcome, &'static str> {
        if !(1..=DISPATCH_GENERATIONS_MAX).contains(&order.delivery_generation)
            || order.offer_digest.as_ref().is_some_and(|digest| {
                digest.len() != 64
                    || !digest
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
        {
            return Err("dispatch_fence_invalid");
        }
        let key = (order.dispatch_id.clone(), order.delivery_generation);
        if let Some((offer, receipt)) = self.denials.get(&key) {
            if order.registration_id != receipt.registration_id
                || order.runner_boot_id != receipt.runner_boot_id
                || order.offer_digest.as_ref() != Some(&offer.offer_digest)
            {
                return Err("dispatch_fence_conflict");
            }
            Ok(FenceOutcome::Denied {
                receipt: Box::new(receipt.clone()),
            })
        } else if order.runner_boot_id == self.identity.runner_id
            && self.next_generation(&order.dispatch_id, order.delivery_generation)
        {
            Ok(FenceOutcome::NotRecordedAtBarrier)
        } else {
            Err("dispatch_fence_origin_unavailable")
        }
    }

    fn validate_fence(
        &self,
        order: &FenceOrder,
        receipt: &FenceReceipt,
        sequence: u64,
    ) -> Result<(), &'static str> {
        if self
            .fences
            .contains_key(&(order.dispatch_id.clone(), order.delivery_generation))
            || self.fences.len() + self.epochs.len() >= EPOCHS_MAX
            || receipt.dispatch_id != order.dispatch_id
            || receipt.delivery_generation != order.delivery_generation
            || receipt.registration_id != order.registration_id
            || receipt.runner_boot_id != order.runner_boot_id
            || receipt.journal_sequence != sequence
            || sequence == 0
            || receipt.outcome != self.fence_outcome(order)?
            || serde_json::to_vec(receipt)
                .map_err(|_| "dispatch_fence_invalid")?
                .len()
                > DISPATCH_RECEIPT_BYTES_MAX
        {
            return Err("dispatch_fence_invalid");
        }
        Ok(())
    }

    pub(super) fn restore_fence(
        &mut self,
        order: FenceOrder,
        receipt: FenceReceipt,
        sequence: u64,
    ) -> io::Result<()> {
        self.validate_fence(&order, &receipt, sequence)
            .map_err(invalid)?;
        let key = (order.dispatch_id.clone(), order.delivery_generation);
        let previous = self.fences.insert(key, (order, receipt));
        assert!(previous.is_none());
        Ok(())
    }

    fn fence_dispatch(
        &mut self,
        ready: &RunnerReady,
        order: FenceOrder,
    ) -> Result<FenceReceipt, &'static str> {
        self.journal_available()?;
        if self.engine_registration.as_ref() != Some(ready)
            || order.registration_id != ready.registration_id
        {
            return Err("dispatch_fence_mismatch");
        }
        let key = (order.dispatch_id.clone(), order.delivery_generation);
        if let Some((original, receipt)) = self.fences.get(&key) {
            if original != &order {
                return Err("dispatch_fence_conflict");
            }
            if matches!(receipt.outcome, FenceOutcome::NotRecordedAtBarrier)
                && order.runner_boot_id != self.identity.runner_id
            {
                return Err("dispatch_fence_origin_unavailable");
            }
            return Ok(receipt.clone());
        }
        let receipt = FenceReceipt {
            dispatch_id: order.dispatch_id.clone(),
            delivery_generation: order.delivery_generation,
            registration_id: order.registration_id.clone(),
            runner_boot_id: order.runner_boot_id.clone(),
            journal_sequence: self.journal.sequence + 1,
            outcome: self.fence_outcome(&order)?,
        };
        self.validate_fence(&order, &receipt, receipt.journal_sequence)?;
        self.append_dispatch(&Record::DispatchFence {
            order: order.clone(),
            receipt: Box::new(receipt.clone()),
        })?;
        let previous = self.fences.insert(key, (order, receipt.clone()));
        assert!(previous.is_none());
        self.assert_consistent();
        assert_eq!(receipt.journal_sequence, self.journal.sequence);
        Ok(receipt)
    }

    pub(super) fn dispatch_status(&self, dispatch_id: &RequestId) -> Reply {
        let key = (
            dispatch_id.clone(),
            self.latest_generation(dispatch_id).unwrap_or(0),
        );
        let receipt = self
            .denials
            .get(&key)
            .map(|(_, receipt)| Box::new(receipt.clone()));
        let fence = self
            .fences
            .get(&key)
            .map(|(_, receipt)| Box::new(receipt.clone()));
        let state = if receipt.is_some() {
            "denied"
        } else if fence.is_some() {
            "not_recorded_at_barrier"
        } else {
            "not_found"
        };
        let reply = Reply::DispatchStatus {
            dispatch_id: dispatch_id.clone(),
            state,
            receipt,
            fence,
        };
        if !matches!(serde_json::to_vec(&reply), Ok(bytes) if bytes.len() <= DISPATCH_VIEW_BYTES_MAX)
        {
            return error("dispatch_status_bounds");
        }
        reply
    }
}

async fn authenticate(
    state: &Arc<Mutex<State>>,
) -> Result<(RunnerClient, RunnerReady), &'static str> {
    let (root, previous) = {
        let mut state = state.lock().await;
        state.journal_available()?;
        let ready = state
            .engine_registration
            .clone()
            .ok_or("engine_registration_unavailable")?;
        (state.journal.root.clone(), ready)
    };
    let client = RunnerClient::new(&root, &previous.registration_id, &previous.boot, TIMEOUT)
        .map_err(|_| "engine_registration_unavailable")?;
    let ready = client
        .ready()
        .await
        .map_err(|_| "engine_registration_unavailable")?;
    {
        let mut state = state.lock().await;
        if let Reply::Error { code } =
            state.register_engine(&previous.registration_id, ready.clone())
        {
            return Err(code);
        }
    }
    Ok((client, ready))
}

async fn status(state: &Arc<Mutex<State>>, dispatch_id: RequestId) -> Reply {
    state
        .lock()
        .await
        .apply(Request {
            protocol_version: super::PROTOCOL_VERSION,
            command: Command::DispatchStatus { dispatch_id },
        })
        .unwrap_or_else(|_| error("journal_unavailable"))
}

pub(super) async fn deliver(
    state: Arc<Mutex<State>>,
    protocol_version: u16,
    dispatch_id: RequestId,
) -> Reply {
    if protocol_version != super::PROTOCOL_VERSION {
        return error("unsupported_version");
    }
    let (client, ready) = match authenticate(&state).await {
        Ok(authenticated) => authenticated,
        Err(code) => return error(code),
    };
    let (retained, generation) = {
        let state = state.lock().await;
        let previous = state.latest_generation(&dispatch_id);
        let retained = previous
            .and_then(|generation| state.denials.get(&(dispatch_id.clone(), generation)))
            .map(|(_, receipt)| receipt.clone());
        (retained, previous.map_or(1, |generation| generation + 1))
    };
    let receipt = match retained {
        Some(receipt) => receipt,
        None => {
            let offer = match client.offer(&dispatch_id, generation).await {
                Ok(offer) => offer,
                Err(_) => return error("engine_offer_unavailable"),
            };
            if offer.dispatch_id != dispatch_id {
                return error("dispatch_offer_mismatch");
            }
            match state.lock().await.consume_offer(&ready, offer) {
                Ok(receipt) => receipt,
                Err(code) => return error(code),
            }
        }
    };
    if client.report_denied(&receipt).await.is_err() {
        return error("dispatch_outcome_unknown");
    }
    status(&state, dispatch_id).await
}

pub(super) async fn reconcile(
    state: Arc<Mutex<State>>,
    protocol_version: u16,
    dispatch_id: RequestId,
) -> Reply {
    if protocol_version != super::PROTOCOL_VERSION {
        return error("unsupported_version");
    }
    let (client, ready) = match authenticate(&state).await {
        Ok(authenticated) => authenticated,
        Err(code) => return error(code),
    };
    let order = match client.reconcile(&dispatch_id).await {
        Ok(order) => order,
        Err(_) => return error("engine_fence_unavailable"),
    };
    if order.dispatch_id != dispatch_id {
        return error("dispatch_fence_mismatch");
    }
    let receipt = match state.lock().await.fence_dispatch(&ready, order) {
        Ok(receipt) => receipt,
        Err(code) => return error(code),
    };
    if client.report_fence(&receipt).await.is_err() {
        return error("dispatch_outcome_unknown");
    }
    status(&state, dispatch_id).await
}

#[cfg(test)]
mod tests {
    use super::super::{Journal, private_directory};
    use super::*;
    use std::mem::{size_of, size_of_val};
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test(flavor = "current_thread")]
    async fn missing_registration_returns_compact_authentication_code() {
        let root = tempfile::Builder::new()
            .prefix("ca-auth-")
            .tempdir_in("/tmp")
            .unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let path = root.path().canonicalize().unwrap();
        let directory = private_directory(&path).unwrap();
        let (journal, records) = Journal::open(&directory, &path).unwrap();
        let state = State::new(journal, records).unwrap();
        assert!(state.engine_registration.is_none());
        let state = Arc::new(Mutex::new(state));
        let failure = match authenticate(&state).await {
            Ok(_) => panic!("unregistered runner authenticated"),
            Err(failure) => failure,
        };
        let error_bytes = size_of_val(&failure);
        let budget_bytes = 2 * size_of::<usize>();
        assert!(
            error_bytes <= budget_bytes,
            "authentication error uses {error_bytes} bytes; two-word budget is {budget_bytes} bytes"
        );
        assert_eq!(
            serde_json::to_value(&failure).unwrap(),
            serde_json::json!("engine_registration_unavailable")
        );
    }
}
