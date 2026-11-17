use super::{Connection, NativeOwner, OwnerEvent, access_problem};
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::owner::{OwnerClient, OwnerClientKind};
use crate::protocol::Topic;
use std::sync::mpsc::{Receiver, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;

/// The most deliveries the watch thread keeps for the run loop, the bound of the channel
/// between the thread and `Connection::changed`. One waiting already makes a refresh due, so
/// a delivery past these is dropped rather than held.
const DELIVERIES_MAX: usize = 8;

/// The TUI's watch thread (v2.5 S3d): it follows the client in its [`Slot`] through the
/// shared feed ([`crate::watch_feed::follow`]), which polls every 5 s while there is none,
/// and hands each delivery's topics to the run loop. Dropping it stops the thread at once.
pub(super) struct Watch {
    deliveries: Receiver<Vec<Topic>>,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    /// Starts the watch thread on a runtime of its own, so a held watch never waits behind
    /// the worker's jobs. Input: the slot the worker fills on attach. Output: the watch, or
    /// the error starting its runtime or thread.
    pub(super) fn start(slot: Slot) -> std::io::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let (sender, deliveries) = sync_channel::<Vec<Topic>>(DELIVERIES_MAX);
        let (stop, stopped) = tokio::sync::oneshot::channel::<()>();
        let thread = std::thread::Builder::new()
            .name("tui-watch".into())
            .spawn(move || {
                let follow = crate::watch_feed::follow(
                    || std::future::ready(slot.current()),
                    |topics: &[Topic]| match sender.try_send(topics.to_vec()) {
                        Ok(()) | Err(TrySendError::Full(_)) => true,
                        Err(TrySendError::Disconnected(_)) => false,
                    },
                );
                runtime.block_on(async move {
                    tokio::select! {
                        () = follow => {}
                        _ = stopped => {}
                    }
                });
            })?;
        Ok(Self {
            deliveries,
            stop: Some(stop),
            thread: Some(thread),
        })
    }
}

impl Drop for Watch {
    /// Ends the follow where it waits, a held watch included, then waits for the thread; a
    /// stop sent to a thread that already ended is ignored.
    /// Input: none. Output: none, once the thread has ended.
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Connection {
    /// The topics the watch delivered since the last call, in order, empty while nothing
    /// committed and no poll period ended. The run loop refreshes once for any.
    /// Input: none. Output: the topics, or an error once the watch thread has stopped.
    pub fn changed(&mut self) -> Result<Vec<Topic>, String> {
        let mut topics = Vec::new();
        loop {
            match self.watch.deliveries.try_recv() {
                Ok(delivered) => topics.extend(delivered),
                Err(TryRecvError::Empty) => return Ok(topics),
                Err(TryRecvError::Disconnected) => return Err("Client watch stopped".into()),
            }
        }
    }
}

/// The owner client the TUI's watch follows (v2.5 S3d): the paired worker's after each
/// attach, none while it is detached, so the watch thread takes a reattached client on its
/// next turn. The worker and the watch thread each hold a clone.
#[derive(Clone, Default)]
pub(super) struct Slot(Arc<Mutex<Option<Arc<OwnerClient>>>>);

impl Slot {
    /// Puts the client to follow in the slot, or none to stop following; the watch thread
    /// takes it on its next turn.
    /// Input: the client. Output: none.
    fn publish(&self, client: Option<Arc<OwnerClient>>) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = client;
    }

    /// The client to follow now, read by the watch thread on each turn of the feed.
    /// Input: none.
    /// Output: the client, none while the worker is detached.
    pub(super) fn current(&self) -> Option<Arc<OwnerClient>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl NativeOwner {
    /// Leaves owner access: the worker is no longer ready, its scan is dropped, its
    /// controller detaches and the watch stops following its client.
    /// Input: none. Output: none.
    pub(super) fn detach(&mut self) {
        self.ready = false;
        self.scan = None;
        if let Some(controller) = &mut self.controller {
            controller.detach();
        }
        self.watch.publish(None);
    }

    /// Attaches as the TUI owner, again on the kept controller after a reconnect, and hands
    /// the attached client to the watch. Input: none. Output: the ordinary client and the
    /// access event, ready or with the plain problem after a detach.
    pub(super) async fn attach(&mut self) -> (Client, OwnerEvent) {
        self.scan = None;
        let ordinary = self.ordinary();
        let result = if let Some(controller) = &mut self.controller {
            controller.reattach_configured(ordinary).await
        } else {
            OwnerController::attach_configured(ordinary, OwnerClientKind::Tui)
                .await
                .map(|(ordinary, controller)| {
                    self.controller = Some(controller);
                    ordinary
                })
        };
        match result {
            Ok(ordinary) => {
                self.ready = true;
                let controller = self.controller.as_ref();
                self.watch
                    .publish(controller.and_then(|c| c.shared_client().ok()));
                (
                    ordinary,
                    OwnerEvent::Access {
                        ready: true,
                        problem: String::new(),
                        identity: controller.map(|c| c.identity().clone()),
                    },
                )
            }
            Err(error) => {
                self.detach();
                (
                    self.ordinary(),
                    OwnerEvent::Access {
                        ready: false,
                        problem: access_problem(&error.0),
                        identity: None,
                    },
                )
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests;
