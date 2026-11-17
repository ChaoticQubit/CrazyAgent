use super::{Routed, Shared, parse, read, routed};
use crazyagent::protocol::Topic;
use crazyagent_desktop::bridge::BridgeError;
use crazyagent_desktop::bridge::watch::Changed;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tokio::sync::Notify;

/// The app's command that starts the window's engine watch, answered here without its
/// channel, which only a native webview has.
pub const WATCH: &str = "desktop_watch";

/// The command the harness's page script long-polls for the next topics in place of the
/// app's watch channel (v2.5 S3c). It lives in this test bridge only and never builds into
/// the app (test/native/e2e-watch.test.mjs).
pub const WATCH_NEXT: &str = "e2e_watch_next";

/// How long one `e2e_watch_next` waits for a change: the feed's own longest watch.
pub const NEXT_WAIT: Duration = Duration::from_millis(crazyagent::watch_feed::WAIT_MS as u64);

/// The bridge's one engine watch, as the app's `Watching` is: whether its feed runs, the
/// topics it handed on that no page took yet, and the newest page's long-poll, which ends an
/// older page's.
#[derive(Default)]
pub struct Watched {
    started: AtomicBool,
    topics: Mutex<BTreeSet<Topic>>,
    changed: Notify,
    waiter: AtomicU64,
}

impl Watched {
    /// Keeps one turn's topics for the next long-poll and wakes it. Input: the topics, at
    /// least one. Output: true, as the feed serves the bridge's life.
    fn add(&self, topics: &[Topic]) -> bool {
        assert!(!topics.is_empty());
        self.topics
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(topics.iter().copied());
        self.changed.notify_waiters();
        true
    }

    /// Waits for topics and takes them all. A newer long-poll ends this one, so a reloaded
    /// page's poll never loses topics to the page it replaced, as the app's channel goes to
    /// the newest page only. Input: the longest wait. Output: the topics, empty when the wait
    /// ran out, or None once a newer poll came.
    pub async fn next(&self, wait: Duration) -> Option<Vec<Topic>> {
        let me = self.waiter.fetch_add(1, Ordering::AcqRel) + 1;
        self.changed.notify_waiters();
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.waiter.load(Ordering::Acquire) != me {
                return None;
            }
            let taken =
                std::mem::take(&mut *self.topics.lock().unwrap_or_else(PoisonError::into_inner));
            if !taken.is_empty() {
                return Some(taken.into_iter().collect());
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Some(Vec::new());
            }
        }
    }
}

/// Answers the page's `desktop_watch` as the app does, minus the channel: the first call
/// starts the bridge's one feed on the shared bridge's owner connection (crazyagent
/// `watch_feed`), and a reload's call starts nothing. Inputs: the shared bridge, the watch
/// and `{ changes }`, the page's channel id. Output: null, or the arguments' refusal.
pub fn watch(state: &Shared, watched: &Arc<Watched>, args: Value) -> Routed {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        changes: String,
    }
    let Args { changes } = match parse(args) {
        Ok(args) => args,
        Err(invalid) => return invalid,
    };
    if !changes.starts_with("__CHANNEL__:") {
        return Routed::Invalid(format!("{WATCH} takes a channel, not {changes:?}"));
    }
    if !watched.started.swap(true, Ordering::AcqRel) {
        let (state, feed) = (Arc::clone(state), Arc::clone(watched));
        tokio::spawn(crazyagent::watch_feed::follow(
            move || {
                let state = Arc::clone(&state);
                async move { read(&state).await.ok()?.watch_client() }
            },
            move |topics: &[Topic]| feed.add(topics),
        ));
    }
    routed(Ok::<_, BridgeError>(Value::Null))
}

/// Answers `e2e_watch_next`: the topics the feed handed on since the last one, waiting up to
/// `NEXT_WAIT` for any. Input: the watch. Output: `{ topics }`, empty when none came, or the
/// failure `watch_superseded` once a newer page polls, which ends the older page's loop.
pub async fn next(watched: &Watched) -> Routed {
    match watched.next(NEXT_WAIT).await {
        Some(topics) => routed(Ok::<_, BridgeError>(Changed { topics })),
        None => Routed::Failure(json!({
            "code": "watch_superseded",
            "message": "a newer page follows the engine watch",
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_poll_takes_every_topic_kept_and_a_newer_poll_ends_an_older_one() {
        let watched = Arc::new(Watched::default());
        assert!(watched.add(&[Topic::Team]));
        assert!(watched.add(&[Topic::Projects, Topic::Team]));
        assert_eq!(
            watched.next(Duration::from_secs(5)).await,
            Some(vec![Topic::Projects, Topic::Team])
        );
        let older = tokio::spawn({
            let watched = Arc::clone(&watched);
            async move { watched.next(Duration::from_secs(30)).await }
        });
        tokio::time::timeout(Duration::from_secs(5), async {
            while watched.waiter.load(Ordering::Acquire) < 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the older poll waits");
        let newer = tokio::spawn({
            let watched = Arc::clone(&watched);
            async move { watched.next(Duration::from_secs(30)).await }
        });
        let ended = tokio::time::timeout(Duration::from_secs(5), older).await;
        assert_eq!(ended.unwrap().unwrap(), None, "a newer poll ends the older");
        assert!(watched.add(&[Topic::Ide]));
        let woken = tokio::time::timeout(Duration::from_secs(5), newer).await;
        assert_eq!(woken.unwrap().unwrap(), Some(vec![Topic::Ide]));
        assert_eq!(
            watched.next(Duration::from_millis(50)).await,
            Some(Vec::new()),
            "a wait that runs out answers no topics"
        );
    }
}
