use crate::engine::EngineError;
use crate::engine::owner::OwnerClient;
use crate::protocol::{Cursor, Topic, WatchReply};
use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long after one reload for changes of unknown subject the next may go out: today's
/// poll period (tui/run.rs `REFRESH`), so engine writes a watch cannot name cost at most one
/// full reload per period (250-S3A), and a client that cannot watch still reloads this often.
pub const UNKNOWN_EVERY: Duration = Duration::from_secs(5);

/// The longest wait one watch asks for: the engine's own cap (owner watch `WAIT_MS_MAX`).
pub const WAIT_MS: u32 = 25_000;

/// The pause after a first failed watch, doubled for each failure in a row up to
/// `UNKNOWN_EVERY`.
const BACKOFF_FIRST: Duration = Duration::from_secs(1);

/// The engine's refusal of a watch past its bound, after which the client polls instead.
const BUSY: &str = "watch_busy";

/// Where one client is in the engine's watch, kept across failed watches and new
/// connections: the cursor and mark the next watch resumes from, when the last reload for
/// changes of unknown subject went out and whether one more is owed, and the failed watches
/// in a row. It does no I/O; `follow` runs it on a connection (v2.5 S3b and S3d).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Feed {
    cursor: Cursor,
    mark: Option<u64>,
    unknown_at: Option<Instant>,
    owed: bool,
    failures: u32,
}

/// What the next watch asks the engine: the changes after `after`, seen from `mark`, waiting
/// at most `wait_ms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ask {
    pub after: Cursor,
    pub mark: Option<u64>,
    pub wait_ms: u32,
}

/// What a client does after one watch: reload what `topics` names, nothing when it is empty,
/// then wait `pause` before the next watch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub topics: Vec<Topic>,
    pub pause: Duration,
}

impl Default for Feed {
    /// A feed that has seen nothing: its first watch starts at cursor 0 with no mark, which
    /// the engine answers at once with `Unknown`, so the client reloads everything once.
    /// Output: the feed.
    fn default() -> Self {
        Self {
            cursor: Cursor::new(0),
            mark: None,
            unknown_at: None,
            owed: false,
            failures: 0,
        }
    }
}

impl Feed {
    /// The next watch to send: after the last cursor and mark, waiting no longer than the end
    /// of the period while an unknown reload is owed, so the owed one goes out on time.
    /// Input: the time now. Output: the request.
    pub fn ask(&self, now: Instant) -> Ask {
        assert!(!self.owed || self.unknown_at.is_some());
        let wait = match self.unknown_at.filter(|_| self.owed) {
            Some(at) => (at + UNKNOWN_EVERY)
                .saturating_duration_since(now)
                .as_nanos()
                .div_ceil(1_000_000),
            None => u128::from(WAIT_MS),
        };
        let wait_ms = u32::try_from(wait).map_or(WAIT_MS, |wait| wait.min(WAIT_MS));
        assert!(wait_ms <= WAIT_MS);
        Ask {
            after: self.cursor.clone(),
            mark: self.mark,
            wait_ms,
        }
    }

    /// Takes one watch's outcome. A reply moves the cursor and mark, and its named topics go
    /// out at once. Its `Unknown`, a refresh and any failed watch go out at most once per
    /// `UNKNOWN_EVERY`, and a failure pauses the next watch. Inputs: the outcome and the
    /// time now. Output: what to reload and how long to wait.
    pub fn answer(&mut self, outcome: Result<WatchReply, EngineError>, now: Instant) -> Turn {
        let code = match outcome {
            Ok(WatchReply::Changed {
                cursor,
                mark,
                topics,
            }) => {
                self.resume(cursor, mark);
                let unknown = topics.contains(&Topic::Unknown);
                let named = topics.into_iter().filter(|topic| *topic != Topic::Unknown);
                return self.turn(named.collect(), unknown, Duration::ZERO, now);
            }
            Ok(WatchReply::Refresh { cursor, mark }) => {
                self.resume(cursor, mark);
                return self.turn(Vec::new(), true, Duration::ZERO, now);
            }
            Ok(WatchReply::Quiet { cursor, mark }) => {
                self.resume(cursor, mark);
                return self.turn(Vec::new(), false, Duration::ZERO, now);
            }
            Ok(WatchReply::Rejected { code }) | Err(EngineError(code)) => code,
        };
        if code == BUSY {
            return self.unwatched(now);
        }
        self.failures = self.failures.saturating_add(1);
        let pause = self.backoff();
        self.turn(Vec::new(), true, pause, now)
    }

    /// Takes a turn with no watch: no connection yet or any more, or `watch_busy`. The client
    /// polls as it did before the watch, reloading everything at most once per
    /// `UNKNOWN_EVERY`, and the cursor and mark stay for the next watch. Input: the time now.
    /// Output: what to reload, and a pause of one period.
    pub fn unwatched(&mut self, now: Instant) -> Turn {
        self.failures = 0;
        self.turn(Vec::new(), true, UNKNOWN_EVERY, now)
    }

    fn resume(&mut self, cursor: Cursor, mark: u64) {
        self.cursor = cursor;
        self.mark = Some(mark);
        self.failures = 0;
    }

    /// The pause after the failed watches in a row: `BACKOFF_FIRST`, doubled for each
    /// failure after the first, never past `UNKNOWN_EVERY`. Input: the feed, with at least
    /// one failure. Output: the pause.
    fn backoff(&self) -> Duration {
        assert!(self.failures > 0);
        let doublings = self.failures.saturating_sub(1).min(3);
        BACKOFF_FIRST
            .saturating_mul(1_u32 << doublings)
            .min(UNKNOWN_EVERY)
    }

    /// Ends a turn: the named topics, then `Unknown` when one is wanted or owed and its
    /// period has passed; one wanted inside the period is owed instead. Inputs: the named
    /// topics, whether an unknown reload is wanted, the pause and the time now. Output: the
    /// turn.
    fn turn(
        &mut self,
        mut topics: Vec<Topic>,
        unknown: bool,
        pause: Duration,
        now: Instant,
    ) -> Turn {
        assert!(!topics.contains(&Topic::Unknown));
        if unknown || self.owed {
            let due = self
                .unknown_at
                .is_none_or(|at| now.saturating_duration_since(at) >= UNKNOWN_EVERY);
            if due {
                topics.push(Topic::Unknown);
                self.unknown_at = Some(now);
            }
            self.owed = !due;
        }
        assert!(!self.owed || self.unknown_at.is_some());
        assert!(pause <= UNKNOWN_EVERY);
        Turn { topics, pause }
    }
}

/// Follows the engine's watch for one client (v2.5 S3b, S3d). Inputs: `client`, asked before
/// each watch for the owner connection to watch on (None while there is none, which polls,
/// and a new one resumes from the last cursor), and `deliver`, given each turn's topics,
/// which answers false once no one receives them. Output: none; it ends with `deliver`'s
/// false, or when the caller drops it.
pub async fn follow<C, F, D>(mut client: C, mut deliver: D)
where
    C: FnMut() -> F,
    F: Future<Output = Option<Arc<OwnerClient>>>,
    D: FnMut(&[Topic]) -> bool,
{
    let mut feed = Feed::default();
    loop {
        let turn = match client().await {
            Some(owner) => {
                let ask = feed.ask(Instant::now());
                let outcome = owner.watch(ask.after, ask.mark, ask.wait_ms).await;
                feed.answer(outcome, Instant::now())
            }
            None => feed.unwatched(Instant::now()),
        };
        if !turn.topics.is_empty() && !deliver(turn.topics.as_slice()) {
            return;
        }
        if !turn.pause.is_zero() {
            tokio::time::sleep(turn.pause).await;
        }
    }
}

#[cfg(test)]
#[path = "watch_feed_tests.rs"]
mod tests;
