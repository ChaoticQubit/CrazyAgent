use super::master_launch::{RcliMasterBinding, RcliMasterLaunch, error};
use super::master_process::HostProcess;
use super::master_reply::{TurnCapture, has_turn};
use super::{
    Control, ControlBody, Event, EventBody, EventSequence, HOST_PROTOCOL_VERSION, RawControl,
};
use crate::protocol::{ProtocolError, RequestId};
use crate::sandbox::InstalledSandbox;
use std::collections::BTreeSet;
use std::io::Cursor;
use std::process::Stdio;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RcliMasterStatus {
    Succeeded,
    Cancelled,
    Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RcliMasterCompletion {
    pub turn_id: RequestId,
    pub status: RcliMasterStatus,
    pub reply: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RcliMasterObservation {
    pub sequence: u64,
    pub event: Event,
    pub completion: Option<RcliMasterCompletion>,
}

impl RcliMasterObservation {
    pub fn is_ready(&self) -> bool {
        matches!(self.event.0.event, EventBody::Ready {})
    }
}

pub struct RcliMasterHost {
    launch: RcliMasterLaunch,
    process: HostProcess,
    sequence: EventSequence,
    started: Instant,
    ready: bool,
    stopped: bool,
    failure: Option<ProtocolError>,
    active: Option<TurnCapture>,
    turn_ids: BTreeSet<RequestId>,
    requests: u64,
}

impl RcliMasterHost {
    pub fn launch(
        launch: RcliMasterLaunch,
        sandbox: &InstalledSandbox,
    ) -> Result<Self, ProtocolError> {
        let mut command = sandbox
            .wrap(&launch.command()?)
            .map_err(|_| error("rcli_master_sandbox_refused"))?;
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        assert_eq!(command.get_program(), sandbox.launcher().as_os_str());
        let process = HostProcess::spawn(command)?;
        let sequence = EventSequence::new(
            launch.binding.session_id.clone(),
            launch.binding.instance_id.clone(),
        );
        let host = Self {
            launch,
            process,
            sequence,
            started: Instant::now(),
            ready: false,
            stopped: false,
            failure: None,
            active: None,
            turn_ids: BTreeSet::new(),
            requests: 0,
        };
        host.launch.command()?;
        assert!(!host.ready);
        assert!(host.active.is_none());
        Ok(host)
    }

    pub fn binding(&self) -> &RcliMasterBinding {
        &self.launch.binding
    }

    /// Names the process group the rcli master runs in, which can outlive this host when
    /// the engine stops. Input: the host. Output: the group.
    pub fn process_group(&self) -> u32 {
        self.process.process_group()
    }

    pub fn prompt(&mut self, turn_id: RequestId, text: &str) -> Result<(), ProtocolError> {
        if !self.ready {
            return Err(error("rcli_master_not_ready"));
        }
        self.available()?;
        if self.active.is_some() || self.turn_ids.contains(&turn_id) || self.turn_ids.len() == 4096
        {
            return Err(error("rcli_master_turn_unavailable"));
        }
        if text.trim().is_empty() || text.len() > 16_384 {
            return Err(error("rcli_master_prompt_bounds"));
        }
        if let Err(failure) = self.launch.validate_profile() {
            return Err(self.fault(failure));
        }
        self.send(ControlBody::Prompt {
            turn_id: turn_id.clone(),
            text: text.into(),
        })?;
        self.turn_ids.insert(turn_id.clone());
        self.active = Some(TurnCapture::new(turn_id));
        assert!(self.active.is_some());
        assert!(self.turn_ids.len() <= 4096);
        Ok(())
    }

    pub fn poll(&mut self) -> Result<Option<RcliMasterObservation>, ProtocolError> {
        self.available()?;
        match self.observe() {
            Ok(value) => Ok(value),
            Err(failure) => Err(self.fault(failure)),
        }
    }

    fn observe(&mut self) -> Result<Option<RcliMasterObservation>, ProtocolError> {
        let Some(bytes) = self.process.frame()? else {
            return Ok(None);
        };
        let frame_bytes = bytes.len();
        let event = super::read_event(&mut Cursor::new(bytes))?;
        self.sequence.accept(&event)?;
        let completion = if matches!(event.0.event, EventBody::Ready {}) {
            self.ready = true;
            None
        } else if let Some(turn) = self.active.as_mut() {
            turn.accept(&event, frame_bytes)?
        } else if has_turn(&event) {
            return Err(error("rcli_master_turn_scope"));
        } else {
            None
        };
        if completion.is_some() {
            self.active = None;
        }
        assert!(self.ready);
        assert_eq!(event.0.sequence, self.sequence.last_sequence());
        Ok(Some(RcliMasterObservation {
            sequence: event.0.sequence,
            event,
            completion,
        }))
    }

    pub fn cancel(&mut self, turn_id: &RequestId) -> Result<(), ProtocolError> {
        self.available()?;
        let turn = self
            .active
            .as_ref()
            .ok_or_else(|| error("rcli_master_turn_unavailable"))?;
        if turn.id != *turn_id || turn.cancelling {
            return Err(error("rcli_master_turn_unavailable"));
        }
        self.send(ControlBody::Cancel {
            turn_id: turn_id.clone(),
        })?;
        let turn = self.active.as_mut().expect("retained active host turn");
        turn.cancelling = true;
        turn.deadline = turn.deadline.min(Instant::now() + Duration::from_secs(6));
        assert_eq!(turn.id, *turn_id);
        assert!(turn.cancelling);
        Ok(())
    }

    pub fn shutdown(&mut self) -> Result<(), ProtocolError> {
        let send_failure = if !self.stopped && self.ready {
            self.send(ControlBody::Shutdown {}).err()
        } else {
            None
        };
        self.stopped = true;
        self.ready = false;
        self.active = None;
        self.process.cleanup()?;
        assert!(self.stopped);
        assert!(self.active.is_none());
        if let Some(failure) = send_failure {
            return Err(failure);
        }
        Ok(())
    }

    fn available(&mut self) -> Result<(), ProtocolError> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        if self.stopped {
            return Err(error("rcli_master_closed"));
        }
        if !self.ready && self.started.elapsed() >= Duration::from_secs(5) {
            return Err(self.fault(error("rcli_master_startup_deadline")));
        }
        if self
            .active
            .as_ref()
            .is_some_and(|turn| Instant::now() >= turn.deadline)
            || self.started.elapsed() >= Duration::from_secs(600)
        {
            return Err(self.fault(error("rcli_master_turn_deadline")));
        }
        Ok(())
    }

    fn send(&mut self, body: ControlBody) -> Result<(), ProtocolError> {
        if self.requests == 4096 {
            return Err(self.fault(error("rcli_master_control_limit")));
        }
        self.requests += 1;
        let control = Control(RawControl {
            protocol_version: HOST_PROTOCOL_VERSION,
            session_id: self.launch.binding.session_id.clone(),
            request_id: RequestId::new(format!("rcli-control-{}", self.requests))?,
            control: body,
        });
        let frame = super::encode_control(&control)?;
        if let Err(failure) = self.process.write(&frame) {
            return Err(self.fault(failure));
        }
        assert!(self.requests <= 4096);
        assert!(!frame.is_empty());
        Ok(())
    }

    fn fault(&mut self, failure: ProtocolError) -> ProtocolError {
        self.stopped = true;
        self.ready = false;
        let failure = if self.process.cleanup().is_err() {
            ProtocolError(format!("{};rcli_master_cleanup_incomplete", failure.0))
        } else {
            failure
        };
        self.failure = Some(failure.clone());
        failure
    }
}
