use super::client::learning::LearningQuery;
use super::client::questions::QuestionsQuery;
use super::frame::Message;
use super::glyphs::Glyphs;
use super::seam::{AccountRow, Arrival, PoolScope, ProjectCard, Request};
use super::theme::{Appearance, Colors, Depth, Theme};
use crate::engine::bootstrap::OwnerConnectionIdentity;
use crate::learning::{LearningProgress, MarkStep};
use crate::owner_profile::OwnerProfile;
use crate::pools::PoolSummary;
use crate::protocol::ProjectId;
use crate::setup::SetupStatus;
use crate::terminal::INPUT_BYTES_MAX;
use ratatui::text::{Line, Span};
use std::collections::VecDeque;

const QUEUE_MAX: usize = 32;
const MARKS_MAX: usize = 4;

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
    pub setup: Load<SetupStatus>,
    pub learning: Load<LearningProgress>,
    pub projects_seen: Option<String>,
    pub questions: Load<Vec<(ProjectId, u32)>>,
    pub waiting: Option<(
        Option<crate::protocol::ProjectId>,
        crate::landing::WaitingChanges,
    )>,
}

impl Store {
    /// Counts the questions waiting for the owner in every project, for the footer badge.
    /// Input: none. Output: the sum of the last counts read, 0 before the first read.
    pub fn pending(&self) -> u64 {
        self.questions.ready().map_or(0, |counts| {
            counts.iter().map(|(_, count)| u64::from(*count)).sum()
        })
    }
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
            setup: Load::Idle,
            learning: Load::Idle,
            projects_seen: None,
            questions: Load::Idle,
            waiting: None,
        }
    }
}

/// Everything a screen needs besides its own state: look, size, engine data,
/// the owner identity of the current trusted connection, whether loading lines
/// are still in their quiet first 150 ms, the owner's EDITOR, the one file waiting
/// for it and whether it runs now, and the queues for requests and messages.
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
    pub hint_rows: u16,
    pub tick: u64,
    pub quiet: bool,
    pub store: Store,
    pub link: Link,
    pub identity: Option<OwnerConnectionIdentity>,
    pub socket: String,
    pub home: Option<String>,
    pub prefs: Option<std::path::PathBuf>,
    pub editor: Option<String>,
    pub editing: Option<super::client::editor::Edit>,
    pub editor_running: std::sync::Arc<std::sync::atomic::AtomicBool>,
    pub sessions_key: Option<(crate::protocol::ProjectId, char)>,
    pub shift_arrows: crate::tui::keys::ShiftArrows,
    pub shift_enter: crate::tui::keys::ShiftEnter,
    pub project_step: Option<(crate::protocol::ProjectId, bool)>,
    requests: VecDeque<Request>,
    refused: Vec<Arrival>,
    reconnect: bool,
    message: Option<Message>,
    generation: u64,
    learned: Vec<MarkStep>,
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
            hint_rows: 1,
            tick: 0,
            quiet: false,
            store: Store::default(),
            link: Link::Connecting,
            identity: None,
            socket,
            home: None,
            prefs: None,
            editor: None,
            editing: None,
            editor_running: std::sync::Arc::default(),
            sessions_key: None,
            shift_arrows: crate::tui::keys::ShiftArrows::default(),
            shift_enter: crate::tui::keys::ShiftEnter::default(),
            project_step: None,
            requests: VecDeque::new(),
            refused: Vec::new(),
            reconnect: false,
            message: None,
            generation: 0,
            learned: Vec::new(),
        }
    }

    /// The rows a wrapped help bar takes from the body past its first row, as the last
    /// frame drew it: 0 unless a settings screen's hints wrap. Input: none. Output: the rows.
    pub fn extra_hint_rows(&self) -> usize {
        assert!(self.hint_rows >= 1);
        usize::from(self.hint_rows - 1)
    }

    /// Rebuilds the theme after the Colors choice or system appearance changed.
    /// Inputs: the new choice and appearance. Output: none.
    pub fn restyle(&mut self, colors: Colors, appearance: Appearance) {
        self.colors = colors;
        self.appearance = appearance;
        self.theme = Theme::new(self.depth, colors, appearance);
    }

    /// Queues one engine request as `try_request` does. A request the full queue refuses
    /// gets the failed reply that request gets from the engine worker, `owner_busy`, kept
    /// until the shell delivers it at the end of the event, so the screen that asked stops
    /// waiting and says so; a refused request with no reply, such as a keystroke, is dropped.
    /// Every reply is kept, with no cap: each one stands for a request this event made, and
    /// the shell takes them all at the end of every event. Input: the request. Output: none.
    pub fn request(&mut self, request: Request) {
        if let Err(request) = self.queue(request)
            && let Some(arrival) = super::run::refused_arrival(*request)
        {
            self.refused.push(arrival);
        }
    }

    /// Queues one engine request for a caller that handles a refusal itself.
    /// Input: the request. Output: whether the engine will see it; false only when the
    /// queue already holds 32 requests and this one was refused.
    pub fn try_request(&mut self, request: Request) -> bool {
        self.queue(request).is_ok()
    }

    /// Takes the busy replies for the requests the full queue refused since the last call,
    /// oldest first. Input: none. Output: the replies.
    pub fn take_refused(&mut self) -> Vec<Arrival> {
        std::mem::take(&mut self.refused)
    }

    /// Queues one engine request; duplicates of a queued read are dropped and
    /// keystrokes for the agent terminal already waiting at the back join it.
    /// Input: the request. Output: Ok when the engine will see it, or the request back
    /// when the queue already holds 32 requests. A joined terminal input holds at most
    /// the engine's terminal input limit.
    fn queue(&mut self, request: Request) -> Result<(), Box<Request>> {
        if matches!(request, Request::Reconnect) {
            self.reconnect = true;
            return Ok(());
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
            return Ok(());
        }
        let counts = |queued: &Request| matches!(queued, Request::Questions(query) if **query == QuestionsQuery::Counts);
        if counts(&request) && self.requests.iter().any(counts) {
            return Ok(());
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
            return Ok(());
        }
        if self.requests.len() >= QUEUE_MAX {
            self.say(Message::error(
                "Too many requests are waiting.",
                "Wait a moment, then try again.",
            ));
            return Err(Box::new(request));
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
        assert!(self.requests.len() <= QUEUE_MAX);
        Ok(())
    }

    /// Takes the next queued request.
    /// Input: none. Output: the oldest request, if any.
    pub fn next_request(&mut self) -> Option<Request> {
        self.requests.pop_front()
    }

    /// Takes the oldest queued request other than the shell's own: the pending-questions
    /// count every refresh queues for the footer badge and the learning marks a page or a
    /// terminal sets, so a test reads what a screen sent. Input: none. Output: that
    /// request, if one waits; the shell's requests stay queued.
    #[cfg(test)]
    pub fn next_screen_request(&mut self) -> Option<Request> {
        let index = self.requests.iter().position(|queued| match queued {
            Request::Questions(query) => **query != QuestionsQuery::Counts,
            Request::Learning(query) => !matches!(
                **query,
                LearningQuery::Mark { step, .. } if step != MarkStep::Skipped
            ),
            _ => true,
        })?;
        self.requests.remove(index)
    }

    /// Takes the oldest queued agent terminal request, ahead of requests
    /// waiting for the engine connection, so typing never waits on a read.
    /// Input: none. Output: the request, if one waits.
    pub fn next_terminal_request(&mut self) -> Option<Request> {
        let index = self.requests.iter().position(Request::is_terminal)?;
        self.requests.remove(index)
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

    /// Clears the message line when it still shows a message said earlier, so any message
    /// said after it stays. Input: the message as it was said. Output: none.
    pub fn unsay(&mut self, said: &Message) {
        if self.message.as_ref() == Some(said) {
            self.message = None;
        }
    }

    /// The message line content while it should still show.
    /// Input: none. Output: the message, or None once a success expired.
    pub fn message(&self) -> Option<&Message> {
        self.message
            .as_ref()
            .filter(|message| message.visible(std::time::Instant::now()))
    }

    /// Sets one learning mark in the background, once per TUI session, for a page or a
    /// terminal the owner opened. Skips an engine without the learning read and a mark the
    /// last read already shows. Input: the mark, never Skipped. Output: none; a mark the
    /// engine refused is tried again the next time (`unlearn`).
    pub fn learn(&mut self, step: MarkStep) {
        assert!(step != MarkStep::Skipped, "only the owner skips the guide");
        let known = match &self.store.learning {
            Load::NotReady => true,
            Load::Ready(progress) => progress.marked(step),
            Load::Idle | Load::Loading | Load::Failed(_) => false,
        };
        if known || self.learned.contains(&step) {
            return;
        }
        self.learned.push(step);
        assert!(self.learned.len() <= MARKS_MAX);
        let generation = self.next_generation();
        self.request(Request::Learning(Box::new(LearningQuery::Mark {
            step,
            on: true,
            generation,
        })));
    }

    /// Forgets a learning mark the engine did not set, so the next visit sends it again.
    /// Input: the mark. Output: none.
    pub fn unlearn(&mut self, step: MarkStep) {
        self.learned.retain(|sent| *sent != step);
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
    fn terminal_requests_jump_the_engine_queue_in_their_own_order() {
        let mut ctx = ctx();
        let agent = crate::terminal::AgentId::team(1).unwrap();
        ctx.request(Request::Projects);
        ctx.request(Request::TerminalInput {
            agent: agent.clone(),
            bytes: b"l".to_vec(),
        });
        ctx.request(Request::TerminalInput {
            agent: agent.clone(),
            bytes: b"s".to_vec(),
        });
        ctx.request(Request::Pools);
        ctx.request(Request::TerminalDetach {
            agent: agent.clone(),
        });
        assert_eq!(ctx.queued(), 4, "typed keys join one input");
        assert!(matches!(
            ctx.next_terminal_request(),
            Some(Request::TerminalInput { bytes, .. }) if bytes == b"ls"
        ));
        assert!(matches!(
            ctx.next_terminal_request(),
            Some(Request::TerminalDetach { .. })
        ));
        assert!(ctx.next_terminal_request().is_none());
        assert!(matches!(ctx.next_request(), Some(Request::Projects)));
        assert!(matches!(ctx.next_request(), Some(Request::Pools)));
    }

    fn full() -> Ctx {
        let mut ctx = ctx();
        while ctx.try_request(pool_removal()) {}
        assert_eq!(ctx.queued(), QUEUE_MAX);
        ctx
    }

    fn pool_removal() -> Request {
        Request::EditPool {
            change: crate::tui::seam::PoolChange::Remove {
                id: crate::protocol::CredentialRef::new("pool-a").unwrap(),
                opened_projects: 0,
            },
        }
    }

    #[test]
    fn every_refused_request_one_event_makes_is_kept_for_its_busy_reply() {
        let mut ctx = full();
        for _ in 0..40 {
            ctx.request(pool_removal());
        }
        assert_eq!(
            ctx.take_refused().len(),
            40,
            "no refused request past the first 32 is dropped without a reply"
        );
        assert!(ctx.take_refused().is_empty());
    }

    #[test]
    fn refused_keystrokes_leave_room_for_the_replies_screens_wait_on() {
        let mut ctx = full();
        let agent = crate::terminal::AgentId::team(1).unwrap();
        for _ in 0..40 {
            ctx.request(Request::TerminalInput {
                agent: agent.clone(),
                bytes: b"x".to_vec(),
            });
        }
        ctx.request(pool_removal());
        assert_eq!(
            ctx.take_refused().len(),
            1,
            "only the pool change waits on a reply"
        );
    }

    #[test]
    fn an_event_refused_over_a_thousand_times_still_answers_each_one_busy() {
        let mut ctx = full();
        for _ in 0..1100 {
            ctx.request(pool_removal());
        }
        assert_eq!(ctx.take_refused().len(), 1100, "no refusal is past a cap");
    }

    #[test]
    fn a_refused_questions_recovery_or_reveal_request_gets_its_busy_reply() {
        use crate::tui::client::OwnerEvent;
        use crate::tui::client::ide::RevealQuery;
        use crate::tui::client::recovery::RecoveryQuery;
        fn busy<T>() -> Result<T, String> {
            Err(crate::tui::seam::OWNER_BUSY.to_owned())
        }
        let project = ProjectId::new("harbor-notes").unwrap();
        let questions = QuestionsQuery::List {
            project: project.clone(),
        };
        let recovery = RecoveryQuery::Read {
            project: project.clone(),
        };
        let reveal = RevealQuery {
            project,
            target: crate::ide::bridge::reveal::RevealTarget::Project {
                path: "src/main.rs".into(),
            },
        };
        let mut ctx = full();
        ctx.request(Request::Questions(Box::new(questions.clone())));
        ctx.request(Request::Recovery(Box::new(recovery.clone())));
        ctx.request(Request::Reveal(Box::new(reveal.clone())));
        let replies: Vec<OwnerEvent> = ctx
            .take_refused()
            .into_iter()
            .map(|arrival| match arrival {
                Arrival::Owner(event) => *event,
                _ => panic!("a refused owner request answers as the owner worker does"),
            })
            .collect();
        assert_eq!(
            replies.len(),
            3,
            "each refused request gets the reply its screen waits on"
        );
        assert!(matches!(
            &replies[0],
            OwnerEvent::Questions(event) if event.query == questions && event.result == busy()
        ));
        assert!(matches!(
            &replies[1],
            OwnerEvent::Recovery(event) if event.query == recovery && event.result == busy()
        ));
        assert!(matches!(
            &replies[2],
            OwnerEvent::Reveal(event) if event.query == reveal && event.result == busy()
        ));
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
            assert!(ctx.try_request(Request::EditPool {
                change: crate::tui::seam::PoolChange::Remove {
                    id: crate::protocol::CredentialRef::new("pool-a").unwrap(),
                    opened_projects: 0,
                },
            }));
        }
        assert_eq!(ctx.queued(), QUEUE_MAX);
        assert!(
            !ctx.try_request(Request::Pools),
            "a refused request tells its caller"
        );
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
