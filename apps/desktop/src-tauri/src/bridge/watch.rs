use super::NativeBridge;
use crazyagent::engine::owner::OwnerClient;
use crazyagent::protocol::Topic;
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use tauri::ipc::Channel;
use tauri::{AppHandle, Manager, Runtime};

/// One message on the window's watch channel (v2.5 S3b): the engine topics whose views the
/// window reloads now, where `unknown` stands for every view it shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Changed {
    pub topics: Vec<Topic>,
}

/// The window's one engine watch: the channel the latest page load gave and whether the task
/// that follows the engine runs. One task serves the app's life, so a page reload replaces
/// the channel and never adds a watch; the desktop holds one engine connection for it
/// (bridge/queue.rs `WATCHES_HELD`).
#[derive(Default)]
pub struct Watching {
    channel: Mutex<Option<Channel<Changed>>>,
    started: AtomicBool,
}

impl Watching {
    /// Gives the watch the channel of the page that just loaded, in place of an earlier
    /// page's. Input: the channel. Output: true the first time only, when the caller starts
    /// the task that follows the engine.
    pub fn listen(&self, channel: Channel<Changed>) -> bool {
        *self.channel.lock().unwrap_or_else(PoisonError::into_inner) = Some(channel);
        !self.started.swap(true, Ordering::AcqRel)
    }

    /// Sends one turn's topics to the page's channel; a channel that refuses them is dropped
    /// until the next page load gives a new one. Input: the topics, at least one. Output:
    /// true, as the watch serves the app's life.
    pub fn deliver(&self, topics: &[Topic]) -> bool {
        assert!(!topics.is_empty());
        let mut channel = self.channel.lock().unwrap_or_else(PoisonError::into_inner);
        let changed = Changed {
            topics: topics.to_vec(),
        };
        if channel
            .as_ref()
            .is_some_and(|open| open.send(changed).is_err())
        {
            *channel = None;
        }
        true
    }
}

impl NativeBridge {
    /// The owner connection the window's engine watch waits on: the controller's shared
    /// client, which a reconnect replaces. Output: the client, or None before the first
    /// connect and after a detach.
    pub fn watch_client(&self) -> Option<Arc<OwnerClient>> {
        self.controller.as_ref()?.shared_client().ok()
    }
}

/// Starts the one task that follows the engine's watch for the window, for the app's life.
/// Before each watch it takes the bridge's owner connection, so a reconnect is followed from
/// the last cursor; with none, or with the bridge busy, it polls (crazyagent `watch_feed`).
/// Input: the app, which manages the bridge and the `Watching`. Output: none.
pub fn follow<R: Runtime>(app: AppHandle<R>) {
    tauri::async_runtime::spawn(async move {
        let reader = app.clone();
        crazyagent::watch_feed::follow(
            move || {
                let app = reader.clone();
                async move {
                    let state = app.state::<crate::NativeState>();
                    let bridge = state.0.read().await.ok()?;
                    bridge.watch_client()
                }
            },
            move |topics: &[Topic]| app.state::<Watching>().deliver(topics),
        )
        .await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use tauri::ipc::InvokeResponseBody;

    /// A channel that keeps what it is sent, as the page would get it, or refuses all.
    /// Input: whether it refuses. Output: the channel and what it got, each message's topics.
    fn page(refuses: bool) -> (Channel<Changed>, Arc<Mutex<Vec<serde_json::Value>>>) {
        let got = Arc::new(Mutex::new(Vec::new()));
        let kept = Arc::clone(&got);
        let channel = Channel::new(move |body| {
            if refuses {
                return Err(tauri::Error::WindowNotFound);
            }
            let InvokeResponseBody::Json(json) = body else {
                panic!("a watch message is JSON");
            };
            kept.lock()
                .unwrap()
                .push(serde_json::from_str(&json).unwrap());
            Ok(())
        });
        (channel, got)
    }

    #[test]
    fn the_first_page_starts_the_watch_and_a_reload_only_replaces_its_channel() {
        let watching = Watching::default();
        let (first, first_got) = page(false);
        let (reloaded, reloaded_got) = page(false);
        assert!(watching.listen(first), "the first page starts the task");
        assert!(watching.deliver(&[Topic::Projects, Topic::Unknown]));
        assert!(!watching.listen(reloaded), "a reload never starts a second");
        assert!(watching.deliver(&[Topic::Settings]));
        assert_eq!(
            *first_got.lock().unwrap(),
            vec![serde_json::json!({ "topics": ["projects", "unknown"] })]
        );
        assert_eq!(
            *reloaded_got.lock().unwrap(),
            vec![serde_json::json!({ "topics": ["settings"] })]
        );
    }

    #[test]
    fn a_channel_that_refuses_is_dropped_and_the_next_page_s_gets_the_topics() {
        let watching = Watching::default();
        let (gone, _) = page(true);
        assert!(watching.listen(gone));
        assert!(watching.deliver(&[Topic::Team]), "the watch goes on");
        assert!(watching.channel.lock().unwrap().is_none());
        assert!(watching.deliver(&[Topic::Team]), "no channel, nothing sent");
        let (next, got) = page(false);
        assert!(!watching.listen(next));
        assert!(watching.deliver(&[Topic::Unknown]));
        assert_eq!(
            *got.lock().unwrap(),
            vec![serde_json::json!({ "topics": ["unknown"] })]
        );
    }
}
