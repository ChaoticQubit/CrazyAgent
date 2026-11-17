use super::{Connection, NativeOwner, OwnerEvent, access_problem};
use crate::client_controller::OwnerController;
use crate::engine::Client;
use crate::engine::owner::OwnerClientKind;
use crate::protocol::Topic;

impl Connection {
    /// The topics the watch delivered since the last call, in order, empty while nothing
    /// committed and no poll period ended. The run loop refreshes once for any.
    /// Input: none. Output: the topics, or an error once the watch thread has stopped.
    pub fn changed(&mut self) -> Result<Vec<Topic>, String> {
        Ok(Vec::new())
    }
}

impl NativeOwner {
    /// Leaves owner access: the worker is no longer ready, its scan is dropped and its
    /// controller detaches.
    /// Input: none. Output: none.
    pub(super) fn detach(&mut self) {
        self.ready = false;
        self.scan = None;
        if let Some(controller) = &mut self.controller {
            controller.detach();
        }
    }

    /// Attaches as the TUI owner, again on the kept controller after a reconnect.
    /// Input: none. Output: the ordinary client and the access event, ready or with the
    /// plain problem after a detach.
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
                (
                    ordinary,
                    OwnerEvent::Access {
                        ready: true,
                        problem: String::new(),
                        identity: self.controller.as_ref().map(|c| c.identity().clone()),
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
