use super::frame::Message;
use super::glyphs::Glyphs;
use super::seam::{AccountRow, PoolScope, ProjectCard, Request};
use super::theme::{Appearance, Colors, Depth, Theme};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::owner_profile::OwnerProfile;
use crate::pools::PoolSummary;
use crate::terminal::INPUT_BYTES_MAX;
use ratatui::text::{Line, Span};
use std::collections::VecDeque;

const QUEUE_MAX: usize = 32;

/// Engine data a screen shows, with its loading state.
/// Input: set by the shell as requests start and replies arrive. Output:
/// screens draw a spinner, the data, the error, or "Not ready yet" when this
/// build has no engine call for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Load<T> {
    Idle,
    Loading,
    Ready(T),
    Failed(String),
    NotReady,
}

impl<T> Load<T> {
    /// The data when it has arrived.
    /// Input: none. Output: the data or None.
    pub fn ready(&self) -> Option<&T> {
        match self {
            Self::Ready(value) => Some(value),
            _ => None,
        }
    }
}

/// Where the shell's engine connection stands.
/// Input: updated from connection events. Output: drives the startup screen,
/// the header status and the reconnect message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Link {
    Connecting,
    Ready,
    Down(String),
    Reconnecting,
}

/// Engine data shared by every screen.
/// Input: filled from replies by the shell. Output: read by screens to draw.
#[derive(Clone, Debug)]
pub struct Store {
    pub profile: Load<Option<OwnerProfile>>,
    pub projects: Load<Vec<ProjectCard>>,
    pub pools: Load<Vec<PoolSummary>>,
    pub pool_projects: Load<Vec<PoolScope>>,
    pub accounts: Load<Vec<AccountRow>>,
    pub harnesses: Load<(usize, usize)>,
    pub memory: Load<usize>,
    pub projects_seen: Option<String>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            profile: Load::Idle,
            projects: Load::Idle,
            pools: Load::Idle,
            pool_projects: Load::Idle,
            accounts: Load::NotReady,
            harnesses: Load::NotReady,
            memory: Load::NotReady,
            projects_seen: None,
        }
    }
}

/// Everything a screen needs besides its own state: look, size, engine data,
/// the owner identity of the current trusted connection, whether loading lines
/// are still in their quiet first 150 ms, and the queues for requests and
/// messages.
/// Input: owned by the shell and lent to screens. Output: requests and the
/// latest message are drained by the shell.
pub struct Ctx {
    pub theme: Theme,
    pub glyphs: &'static Glyphs,
    pub depth: Depth,
    pub colors: Colors,
    pub appearance: Appearance,
    pub width: u16,
    pub height: u16,
    pub tick: u64,
    pub quiet: bool,
    pub store: Store,
    pub link: Link,
    pub identity: Option<OwnerConnectionIdentity>,
    pub socket: String,
    pub home: Option<String>,
    pub prefs: Option<std::path::PathBuf>,
    requests: VecDeque<Request>,
    reconnect: bool,
    message: Option<Message>,
    generation: u64,
}

impl Ctx {
    /// Builds the context for one terminal.
    /// Inputs: depth, Colors choice, system appearance, glyphs and the engine
    /// socket path shown when the engine is down. Output: a context with no data.
    pub fn new(
        depth: Depth,
        colors: Colors,
        appearance: Appearance,
        glyphs: &'static Glyphs,
        socket: String,
    ) -> Self {
        Self {
            theme: Theme::new(depth, colors, appearance),
            glyphs,
            depth,
            colors,
            appearance,
            width: 120,
            height: 36,
            tick: 0,
            quiet: false,
            store: Store::default(),
            link: Link::Connecting,
            identity: None,
            socket,
            home: None,
            prefs: None,
            requests: VecDeque::new(),
            reconnect: false,
            message: None,
            generation: 0,
        }
    }

    /// Rebuilds the theme after the Colors choice or system appearance changed.
    /// Inputs: the new choice and appearance. Output: none.
    pub fn restyle(&mut self, colors: Colors, appearance: Appearance) {
        self.colors = colors;
        self.appearance = appearance;
        self.theme = Theme::new(self.depth, colors, appearance);
    }

    /// Queues one engine request; duplicates of a queued read are dropped and
    /// keystrokes for the agent terminal already waiting at the back join it.
    /// Input: the request. Output: none; the queue holds at most 32 requests,
    /// a joined terminal input at most the engine's terminal input limit.
    pub fn request(&mut self, request: Request) {
        if matches!(request, Request::Reconnect) {
            self.reconnect = true;
            return;
        }
        if let Request::TerminalInput { agent, bytes } = &request
            && let Some(Request::TerminalInput {
                agent: waiting,
                bytes: queued,
            }) = self.requests.back_mut()
            && waiting == agent
            && queued.len() + bytes.len() <= INPUT_BYTES_MAX
        {
            queued.extend_from_slice(bytes);
            return;
        }
        let read = matches!(
            request,
            Request::Profile | Request::Pools | Request::PoolProjects | Request::Projects
        );
        if read
            && self
                .requests
                .iter()
                .any(|queued| std::mem::discriminant(queued) == std::mem::discriminant(&request))
        {
            return;
        }
        if self.requests.len() >= QUEUE_MAX {
            self.say(Message::error(
                "Too many requests are waiting.",
                "Wait a moment, then try again.",
            ));
            return;
        }
        match request {
            Request::Profile => self.store.profile = Load::Loading,
            Request::Pools if self.store.pools.ready().is_none() => {
                self.store.pools = Load::Loading
            }
            Request::PoolProjects if self.store.pool_projects.ready().is_none() => {
                self.store.pool_projects = Load::Loading
            }
            Request::Projects if self.store.projects.ready().is_none() => {
                self.store.projects = Load::Loading
            }
            _ => {}
        }
        self.requests.push_back(request);
    }

    /// Takes the next queued request.
    /// Input: none. Output: the oldest request, if any.
    pub fn next_request(&mut self) -> Option<Request> {
        self.requests.pop_front()
    }

    /// Takes a pending reconnect ask; it goes ahead of queued requests,
    /// which wait for the connection.
    /// Input: none. Output: true once per ask.
    pub fn take_reconnect(&mut self) -> bool {
        std::mem::take(&mut self.reconnect)
    }

    /// Number of queued requests.
    /// Input: none. Output: the count.
    pub fn queued(&self) -> usize {
        self.requests.len()
    }

    /// Shows a result on the message line, replacing the previous one.
    /// Input: the message. Output: none.
    pub fn say(&mut self, message: Message) {
        self.message = Some(message);
    }

    /// The message line content while it should still show.
    /// Input: none. Output: the message, or None once a success expired.
    pub fn message(&self) -> Option<&Message> {
        self.message
            .as_ref()
            .filter(|message| message.visible(std::time::Instant::now()))
    }

    /// A fresh nonzero client generation for one owner mutation.
    /// Input: none. Output: the next generation number.
    pub fn next_generation(&mut self) -> u64 {
        self.generation = self
            .generation
            .checked_add(1)
            .expect("client generation stays far below u64::MAX");
        assert!(self.generation > 0);
        self.generation
    }

    /// The spinner frame for this tick.
    /// Input: none. Output: one glyph.
    pub fn spinner(&self) -> &'static str {
        self.glyphs.spin(self.tick)
    }

    /// One loading line: the spinner and what is loading, or an empty line
    /// during the quiet first 150 ms of a busy spell.
    /// Input: what is loading, such as "Loading projects". Output: the line.
    pub fn loading(&self, text: &str) -> Line<'static> {
        if self.quiet {
            return Line::default();
        }
        Line::from(vec![
            Span::styled(self.spinner(), self.theme.running),
            Span::raw(format!(" {text}")),
        ])
    }

    /// Says whether the window uses the wide layouts.
    /// Input: none. Output: true at 100 columns and up.
    pub fn wide(&self) -> bool {
        self.width >= 100
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::glyphs::UNICODE;

    fn ctx() -> Ctx {
        Ctx::new(
            Depth::NoColor,
            Colors::Dark,
            Appearance::Dark,
            &UNICODE,
            String::new(),
        )
    }

    #[test]
    fn queued_reads_are_not_repeated_and_reconnect_jumps_the_queue() {
        let mut ctx = ctx();
        ctx.request(Request::Projects);
        ctx.request(Request::Pools);
        ctx.request(Request::Projects);
        ctx.request(Request::Reconnect);
        assert_eq!(ctx.queued(), 2);
        assert_eq!(ctx.store.projects, Load::Loading);
        assert!(ctx.take_reconnect());
        assert!(!ctx.take_reconnect(), "one ask, one reconnect");
        assert!(matches!(ctx.next_request(), Some(Request::Projects)));
        ctx.request(Request::Projects);
        assert_eq!(ctx.queued(), 2, "a sent read may be asked for again");
    }

    #[test]
    fn a_refresh_keeps_shown_data_instead_of_a_spinner() {
        let mut ctx = ctx();
        ctx.store.pools = Load::Ready(Vec::new());
        ctx.request(Request::Pools);
        assert_eq!(ctx.store.pools, Load::Ready(Vec::new()));
        ctx.request(Request::Profile);
        assert_eq!(ctx.store.profile, Load::Loading);
    }

    #[test]
    fn the_queue_is_bounded_and_says_so() {
        let mut ctx = ctx();
        for _ in 0..QUEUE_MAX {
            ctx.request(Request::EditPool {
                change: crate::tui::seam::PoolChange::Remove {
                    id: crate::protocol::CredentialRef::new("pool-a").unwrap(),
                },
            });
        }
        assert_eq!(ctx.queued(), QUEUE_MAX);
        ctx.request(Request::Pools);
        assert_eq!(ctx.queued(), QUEUE_MAX);
        assert_eq!(
            ctx.message().unwrap().lead,
            "Too many requests are waiting."
        );
        assert_eq!(
            ctx.store.pools,
            Load::Idle,
            "a refused read leaves the data alone"
        );
    }

    #[test]
    fn generations_are_nonzero_and_increase() {
        let mut ctx = ctx();
        let first = ctx.next_generation();
        assert!(first > 0);
        assert!(ctx.next_generation() > first);
    }
}
