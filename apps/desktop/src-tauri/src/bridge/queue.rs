use super::{BridgeError, NativeBridge};
use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{OwnedRwLockWriteGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

const BRIDGE_WAIT: Duration = Duration::from_secs(2);
const BRIDGE_WAITERS_MAX: usize = 32;
const BRIDGE_READERS_MAX: u32 = 4;

/// The one native bridge the window's calls share. A call that changes the bridge
/// or the engine has it alone; plain reads share it, at most `BRIDGE_READERS_MAX`
/// at once. Calls wait their turn first come first served, so a read asked after a
/// change sees it, for at most `BRIDGE_WAIT`, with at most `BRIDGE_WAITERS_MAX`
/// calls waiting at once. Past either bound the call is refused with `busy` before
/// it does anything.
pub struct BridgeQueue {
    bridge: Arc<RwLock<NativeBridge>>,
    waiting: AtomicUsize,
}

impl BridgeQueue {
    /// Puts the bridge behind the queue. Input: the bridge. Output: the queue.
    pub fn new(bridge: NativeBridge) -> Self {
        Self {
            bridge: Arc::new(RwLock::with_max_readers(bridge, BRIDGE_READERS_MAX)),
            waiting: AtomicUsize::new(0),
        }
    }

    /// Takes the bridge alone for one call. Output: the bridge, or `busy` when
    /// the queue is full or the wait ran out.
    pub async fn enter(&self) -> Result<RwLockWriteGuard<'_, NativeBridge>, BridgeError> {
        self.queued(self.bridge.write()).await
    }

    /// Takes the bridge alone for a call that hands it to a spawned task, with
    /// the same bounds as `enter`. Output: the owned bridge, or `busy`.
    pub async fn enter_owned(&self) -> Result<OwnedRwLockWriteGuard<NativeBridge>, BridgeError> {
        self.queued(Arc::clone(&self.bridge).write_owned()).await
    }

    /// Shares the bridge with other plain reads, with the same bounds as `enter`.
    /// Output: the shared bridge, or `busy`.
    pub async fn read(&self) -> Result<RwLockReadGuard<'_, NativeBridge>, BridgeError> {
        self.queued(self.bridge.read()).await
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
    async fn plain_reads_share_the_bridge_up_to_the_reader_bound() {
        let queue = Arc::new(BridgeQueue::new(NativeBridge::new(None)));
        let mut reads = Vec::new();
        for _ in 0..BRIDGE_READERS_MAX {
            let asked = Instant::now();
            reads.push(queue.read().await.unwrap());
            assert!(asked.elapsed() < HOLD, "a read never waits on another read");
        }
        let extra = tokio::spawn({
            let queue = Arc::clone(&queue);
            async move { queue.read().await.map(drop).map_err(|e| e.code) }
        });
        tokio::time::sleep(HOLD).await;
        assert!(!extra.is_finished(), "a read past the bound waits");
        reads.pop();
        assert_eq!(extra.await.unwrap(), Ok(()));
    }

    #[tokio::test]
    async fn a_change_waits_for_running_reads_and_reads_asked_after_it_wait_for_it() {
        let queue = Arc::new(BridgeQueue::new(NativeBridge::new(None)));
        let order = Arc::new(std::sync::Mutex::new(Vec::new()));
        let first = queue.read().await.unwrap();
        let mut calls = Vec::new();
        for (index, change) in [(0, true), (1, false), (2, false), (3, true), (4, false)] {
            let queue = Arc::clone(&queue);
            let order = Arc::clone(&order);
            calls.push(tokio::spawn(async move {
                if change {
                    let bridge = queue.enter().await.map_err(|e| e.code)?;
                    order.lock().unwrap().push(index);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    drop(bridge);
                } else {
                    let bridge = queue.read().await.map_err(|e| e.code)?;
                    order.lock().unwrap().push(index);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    drop(bridge);
                }
                Ok::<(), &str>(())
            }));
            tokio::task::yield_now().await;
        }
        tokio::time::sleep(HOLD).await;
        assert!(
            order.lock().unwrap().is_empty(),
            "the change waits for the running read"
        );
        drop(first);
        for call in calls {
            assert_eq!(call.await.unwrap(), Ok(()));
        }
        let order = order.lock().unwrap().clone();
        assert_eq!(order[0], 0, "the first change goes first");
        assert_eq!(
            order[1..3]
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>(),
            [1, 2].into(),
            "the reads asked after it run after it, together"
        );
        assert_eq!(&order[3..], &[3, 4]);
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
