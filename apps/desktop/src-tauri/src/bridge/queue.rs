use super::{BridgeError, NativeBridge};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, MutexGuard, OwnedMutexGuard};

const BRIDGE_WAIT: Duration = Duration::from_secs(2);
const BRIDGE_WAITERS_MAX: usize = 32;

/// The one native bridge the window's calls share. A call that finds it in use
/// waits its turn, first come first served, for at most `BRIDGE_WAIT`, with at
/// most `BRIDGE_WAITERS_MAX` calls waiting at once. Past either bound the call
/// is refused with `busy` before it does anything.
pub struct BridgeQueue {
    bridge: Arc<Mutex<NativeBridge>>,
    waiting: AtomicUsize,
}

impl BridgeQueue {
    /// Puts the bridge behind the queue. Input: the bridge. Output: the queue.
    pub fn new(bridge: NativeBridge) -> Self {
        Self {
            bridge: Arc::new(Mutex::new(bridge)),
            waiting: AtomicUsize::new(0),
        }
    }

    /// Takes the bridge for one call. Output: the bridge, or `busy` when the
    /// queue is full or the wait ran out.
    pub async fn enter(&self) -> Result<MutexGuard<'_, NativeBridge>, BridgeError> {
        self.queued(self.bridge.lock()).await
    }

    /// Takes the bridge for a call that hands it to a spawned task, with the
    /// same bounds as `enter`. Output: the owned bridge, or `busy`.
    pub async fn enter_owned(&self) -> Result<OwnedMutexGuard<NativeBridge>, BridgeError> {
        self.queued(Arc::clone(&self.bridge).lock_owned()).await
    }

    async fn queued<G>(&self, lock: impl Future<Output = G>) -> Result<G, BridgeError> {
        let _turn = Turn::take(&self.waiting)?;
        tokio::time::timeout(BRIDGE_WAIT, lock)
            .await
            .map_err(|_| super::failure("busy"))
    }
}

/// One call's place among the calls waiting for the bridge, given back when the
/// call gets the bridge, gives up or is dropped.
struct Turn<'a>(&'a AtomicUsize);

impl<'a> Turn<'a> {
    fn take(waiting: &'a AtomicUsize) -> Result<Self, BridgeError> {
        waiting
            .try_update(Ordering::AcqRel, Ordering::Acquire, |count| {
                (count < BRIDGE_WAITERS_MAX).then_some(count + 1)
            })
            .map(|_| Self(waiting))
            .map_err(|_| super::failure("busy"))
    }
}

impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const HOLD: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn a_call_waits_its_turn_while_another_call_holds_the_bridge() {
        let queue = Arc::new(BridgeQueue::new(NativeBridge::new(None)));
        let held = queue.enter().await.unwrap();
        let waiter = tokio::spawn({
            let queue = Arc::clone(&queue);
            async move { queue.enter().await.map(drop).map_err(|e| e.code) }
        });
        tokio::time::sleep(HOLD).await;
        drop(held);
        assert_eq!(waiter.await.unwrap(), Ok(()));
        let owned = queue.enter_owned().await.unwrap();
        let waiter = tokio::spawn({
            let queue = Arc::clone(&queue);
            async move { queue.enter_owned().await.map(drop).map_err(|e| e.code) }
        });
        tokio::time::sleep(HOLD).await;
        drop(owned);
        assert_eq!(waiter.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn a_burst_of_calls_all_get_the_bridge_in_arrival_order() {
        let queue = Arc::new(BridgeQueue::new(NativeBridge::new(None)));
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let held = queue.enter().await.unwrap();
        let mut calls = Vec::new();
        for call in 0..BRIDGE_WAITERS_MAX {
            let queue = Arc::clone(&queue);
            let order = Arc::clone(&order);
            calls.push(tokio::spawn(async move {
                let bridge = queue.enter().await.map_err(|e| e.code)?;
                order.lock().unwrap().push(call);
                drop(bridge);
                Ok::<(), &str>(())
            }));
            tokio::task::yield_now().await;
        }
        drop(held);
        for call in calls {
            assert_eq!(call.await.unwrap(), Ok(()));
        }
        assert_eq!(
            *order.lock().unwrap(),
            (0..BRIDGE_WAITERS_MAX).collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn a_call_past_the_waiting_bound_is_refused_at_once() {
        let queue = Arc::new(BridgeQueue::new(NativeBridge::new(None)));
        let held = queue.enter().await.unwrap();
        let mut calls = Vec::new();
        for _ in 0..BRIDGE_WAITERS_MAX {
            let queue = Arc::clone(&queue);
            calls.push(tokio::spawn(async move {
                queue.enter().await.map(drop).map_err(|e| e.code)
            }));
            tokio::task::yield_now().await;
        }
        let asked = Instant::now();
        assert_eq!(
            queue.enter().await.map(drop).map_err(|e| e.code),
            Err("busy")
        );
        assert!(
            asked.elapsed() < HOLD,
            "the extra call was refused without waiting"
        );
        drop(held);
        for call in calls {
            assert_eq!(call.await.unwrap(), Ok(()));
        }
        assert_eq!(queue.enter().await.map(drop).map_err(|e| e.code), Ok(()));
    }

    #[tokio::test]
    async fn a_call_that_waits_past_the_time_bound_is_refused_busy() {
        let queue = Arc::new(BridgeQueue::new(NativeBridge::new(None)));
        let held = queue.enter().await.unwrap();
        let asked = Instant::now();
        assert_eq!(
            queue.enter().await.map(drop).map_err(|e| e.code),
            Err("busy")
        );
        assert!(
            asked.elapsed() >= BRIDGE_WAIT,
            "the call waited its full turn first"
        );
        assert!(asked.elapsed() < BRIDGE_WAIT * 2);
        drop(held);
        assert_eq!(queue.enter().await.map(drop).map_err(|e| e.code), Ok(()));
    }
}
