use super::{Routed, Shared, lock, parse, read, routed};
use crazyagent::terminal::TerminalRequest;
use crazyagent_desktop::bridge::BridgeError;
use crazyagent_desktop::bridge::terminal::{
    Session, Sink, Terminals, graph_shows, input_request, place_project, view_key,
};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;
use tauri::ipc::{Channel, InvokeResponseBody};
use tokio::sync::Notify;

/// The app's command that opens a live view of an agent terminal on a channel.
pub const ATTACH: &str = "desktop_terminal_attach";
/// The app's command that types a view's keys into its terminal.
pub const INPUT: &str = "desktop_terminal_input";
/// The app's command that sends the owner's prompt to a terminal.
pub const PROMPT: &str = "desktop_terminal_prompt";
/// The app's command that sizes a terminal.
pub const RESIZE: &str = "desktop_terminal_resize";
/// The app's command that closes a view; the agent keeps running.
pub const DETACH: &str = "desktop_terminal_detach";
/// The command the harness's page script long-polls for a view's next frames in place of
/// the app's terminal channel. It lives in this test bridge only.
pub const NEXT: &str = "e2e_terminal_next";

/// The one window the bridge's page is, as the app labels its main window.
const WINDOW: &str = "main";
/// How long one `e2e_terminal_next` waits for a frame.
const NEXT_WAIT: Duration = Duration::from_secs(10);
/// The most frames one view keeps that no poll took yet; past it the view's channel refuses,
/// which ends the view as a closed page does in the app.
const FRAMES_MAX: usize = 4096;
/// The most views the bridge keeps frames for at once.
const QUEUES_MAX: usize = 64;

/// One view's frames that no poll took yet, and whether its last one came.
#[derive(Default)]
struct Queue {
    frames: VecDeque<Value>,
    ended: bool,
}

/// The bridge's terminal views: the app's own `Terminals`, which attach, type, size and
/// detach exactly as the app does, and each view's frames by its page channel id.
#[derive(Default)]
pub struct Views {
    terminals: Arc<Terminals>,
    queues: Mutex<HashMap<u32, Queue>>,
    changed: Notify,
}

impl Views {
    /// Keeps one frame for the channel's next poll and wakes it. Inputs: the channel id and
    /// the frame. Output: ok, or an error once the queue is full or gone, which the app's
    /// reader takes as a closed page.
    fn push(&self, channel: u32, body: InvokeResponseBody) -> Result<(), ()> {
        let (frame, last) = match body {
            InvokeResponseBody::Raw(bytes) => (json!({ "raw": bytes }), false),
            InvokeResponseBody::Json(text) => {
                let value: Value = serde_json::from_str(&text).map_err(|_| ())?;
                let last = value.get("kind").and_then(Value::as_str) != Some("attached");
                (json!({ "json": value }), last)
            }
        };
        let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
        let queue = queues.get_mut(&channel).ok_or(())?;
        if queue.frames.len() >= FRAMES_MAX {
            return Err(());
        }
        queue.frames.push_back(frame);
        queue.ended |= last;
        drop(queues);
        self.changed.notify_waiters();
        Ok(())
    }

    /// Waits for a channel's frames and takes them all. Inputs: the channel id and the
    /// longest wait. Output: the frames and whether the view ended, or None for a channel
    /// the bridge never opened or already forgot.
    async fn next(&self, channel: u32, wait: Duration) -> Option<(Vec<Value>, bool)> {
        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let notified = self.changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            {
                let mut queues = self.queues.lock().unwrap_or_else(PoisonError::into_inner);
                let queue = queues.get_mut(&channel)?;
                if !queue.frames.is_empty() || queue.ended {
                    let frames: Vec<Value> = queue.frames.drain(..).collect();
                    let ended = queue.ended;
                    if ended {
                        queues.remove(&channel);
                    }
                    return Some((frames, ended));
                }
            }
            if tokio::time::timeout_at(deadline, notified).await.is_err() {
                return Some((Vec::new(), false));
            }
        }
    }
}

/// The channel id a page names as `__CHANNEL__:<id>`. Input: the argument. Output: the id.
fn channel_id(output: &str) -> Option<u32> {
    output.strip_prefix("__CHANNEL__:")?.parse().ok()
}

/// Answers the page's `desktop_terminal_attach` as the app's attach does on Windows and
/// Linux: the view key, the window's project check through the team graph, a place among the
/// window's views, a real engine session and `Reservation::attach` on a channel whose frames
/// the page polls. Inputs: the shared bridge, the views and `{ agent, pane, output, id }`.
/// Output: the view's serial, or the app's refusal.
pub async fn attach(state: &Shared, views: &Arc<Views>, args: Value) -> Routed {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        agent: String,
        pane: Option<String>,
        output: String,
        id: Option<String>,
    }
    let Args {
        agent,
        pane,
        output,
        id,
    } = match parse(args) {
        Ok(args) => args,
        Err(invalid) => return invalid,
    };
    let Some(channel) = channel_id(&output) else {
        return Routed::Invalid(format!("{ATTACH} takes a channel, not {output:?}"));
    };
    let result: Result<u64, BridgeError> = async {
        let key = view_key(&agent, pane.as_deref())?;
        let project = place_project(WINDOW, id.as_deref())?;
        let shows = async {
            if let Some(id) = project {
                let graph = read(state).await?.team_graph(&id).await?;
                if !graph_shows(&graph, &key.agent) {
                    return Err(BridgeError {
                        code: "window_denied",
                        cause: None,
                        account: None,
                    });
                }
            }
            Ok::<(), BridgeError>(())
        };
        let place = views.terminals.checked_room(WINDOW, &key, shows).await?;
        let session = lock(state)
            .await?
            .open_terminal(key.agent.clone(), key.pane.clone(), None, Session::Full)
            .await?;
        {
            let mut queues = views.queues.lock().unwrap_or_else(PoisonError::into_inner);
            if queues.len() >= QUEUES_MAX && !queues.contains_key(&channel) {
                return Err(BridgeError {
                    code: "terminal_views_full",
                    cause: None,
                    account: None,
                });
            }
            queues.insert(channel, Queue::default());
        }
        let feed = Arc::clone(views);
        let sink = Sink::Page(Channel::new(move |body| {
            feed.push(channel, body)
                .map_err(|()| tauri::Error::FailedToReceiveMessage)
        }));
        place.attach(session, sink)
    }
    .await;
    routed(result)
}

/// Answers the page's typing, prompt and resize calls through the app's own `Terminals`,
/// which checks each request and queues it for the view. Inputs: the views, the command and
/// its arguments. Output: null, or the app's refusal.
pub async fn send(views: &Views, command: &str, args: Value) -> Routed {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        agent: String,
        pane: Option<String>,
        bytes: Option<Vec<u8>>,
        wheel: Option<bool>,
        text: Option<String>,
        cols: Option<u16>,
        rows: Option<u16>,
    }
    let Args {
        agent,
        pane,
        bytes,
        wheel,
        text,
        cols,
        rows,
    } = match parse(args) {
        Ok(args) => args,
        Err(invalid) => return invalid,
    };
    let request = match (command, bytes, text, cols, rows) {
        (INPUT, Some(bytes), None, None, None) => input_request(bytes, wheel.unwrap_or(false)),
        (PROMPT, None, Some(text), None, None) if wheel.is_none() => {
            TerminalRequest::Prompt { text }
        }
        (RESIZE, None, None, Some(cols), Some(rows)) if wheel.is_none() => {
            TerminalRequest::Resize { cols, rows }
        }
        _ => return Routed::Invalid(format!("{command} arguments do not match the app's")),
    };
    let result: Result<(), BridgeError> = async {
        let key = view_key(&agent, pane.as_deref())?;
        views.terminals.send(WINDOW, &key, request).await
    }
    .await;
    routed(result)
}

/// Answers the page's `desktop_terminal_detach` through the app's own `Terminals`. Inputs:
/// the views and `{ agent, serial }`. Output: null, or the app's refusal.
pub fn detach(views: &Views, args: Value) -> Routed {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        agent: String,
        serial: u64,
    }
    let Args { agent, serial } = match parse(args) {
        Ok(args) => args,
        Err(invalid) => return invalid,
    };
    let result: Result<(), BridgeError> = crazyagent_desktop::bridge::terminal::agent(&agent)
        .and_then(|agent| views.terminals.detach(WINDOW, &agent, serial));
    routed(result)
}

/// Answers `e2e_terminal_next`: the frames a view's channel got since the last poll, each
/// `{ json }` or `{ raw }`, waiting up to `NEXT_WAIT` for any. Inputs: the views and
/// `{ channel }`. Output: `{ frames, ended }`, or the failure `terminal_not_attached` for a
/// channel the bridge does not follow.
pub async fn next(views: &Views, args: Value) -> Routed {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Args {
        channel: u32,
    }
    let Args { channel } = match parse(args) {
        Ok(args) => args,
        Err(invalid) => return invalid,
    };
    match views.next(channel, NEXT_WAIT).await {
        Some((frames, ended)) => Routed::Value(json!({ "frames": frames, "ended": ended })),
        None => Routed::Failure(json!({
            "code": "terminal_not_attached",
            "message": "the bridge follows no terminal on that channel",
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_poll_takes_a_views_frames_in_order_and_its_last_frame_ends_it() {
        let views = Views::default();
        assert!(
            views.push(7, InvokeResponseBody::Raw(vec![1])).is_err(),
            "no queue, no frame"
        );
        views.queues.lock().unwrap().insert(7, Queue::default());
        let attached = r#"{"kind":"attached","agent_id":"master:1","cols":80,"rows":24}"#;
        views
            .push(7, InvokeResponseBody::Json(attached.to_owned()))
            .unwrap();
        views
            .push(7, InvokeResponseBody::Raw(b"qf".to_vec()))
            .unwrap();
        let (frames, ended) = views.next(7, Duration::from_secs(5)).await.unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0]["json"]["kind"], "attached");
        assert_eq!(frames[1]["raw"], json!([113, 102]));
        assert!(!ended);
        assert_eq!(
            views.next(7, Duration::from_millis(50)).await,
            Some((Vec::new(), false))
        );
        let closed = r#"{"kind":"closed","reason":"terminal_closed"}"#;
        views
            .push(7, InvokeResponseBody::Json(closed.to_owned()))
            .unwrap();
        let (frames, ended) = views.next(7, Duration::from_secs(5)).await.unwrap();
        assert_eq!((frames.len(), ended), (1, true));
        assert_eq!(
            views.next(7, Duration::from_millis(50)).await,
            None,
            "an ended view is forgotten"
        );
        views.queues.lock().unwrap().insert(8, Queue::default());
        for _ in 0..FRAMES_MAX {
            views.push(8, InvokeResponseBody::Raw(vec![0])).unwrap();
        }
        assert!(
            views.push(8, InvokeResponseBody::Raw(vec![0])).is_err(),
            "a full queue refuses"
        );
        assert_eq!(channel_id("__CHANNEL__:12"), Some(12));
        assert_eq!(channel_id("12"), None);
    }
}
