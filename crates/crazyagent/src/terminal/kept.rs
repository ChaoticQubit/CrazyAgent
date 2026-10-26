use super::buffer::buffer;
use super::{AgentId, Host, HostError, PaneRef, TerminalStream, VIEWER_BUFFER_BYTES_MAX};
use crate::harnesses::interactive::InteractiveCommand;
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// How many ended terminals keep their last screen: two full projects of agents. Past it the
/// screen kept longest ago is forgotten first.
const KEPT_MAX: usize = 32;
/// The largest last screen, label included, a host keeps; a larger screen is not kept.
const KEPT_BYTES_MAX: usize = VIEWER_BUFFER_BYTES_MAX;

/// One ended terminal's last screen as a viewer redraws it, with the size it had.
#[derive(Debug)]
struct Kept {
    agent: AgentId,
    cols: u16,
    rows: u16,
    screen: Vec<u8>,
}

/// The last screens of the terminals a host ended, newest last, at most `KEPT_MAX`.
#[derive(Debug, Default)]
struct LastScreens {
    kept: VecDeque<Kept>,
}

impl LastScreens {
    /// Keeps an ended terminal's screen with its label inserted as the top row, which pushes
    /// the bottom row off, replacing what that agent had kept. Inputs: the agent, the screen
    /// as the host redraws it, its size and the label. Output: none; a screen past
    /// `KEPT_BYTES_MAX` is not kept and the agent's older one is dropped all the same.
    fn keep(&mut self, agent: &AgentId, screen: &[u8], cols: u16, rows: u16, label: &str) {
        self.forget(agent);
        let text: String = label.chars().take(usize::from(cols)).collect();
        let mut kept = screen.to_vec();
        kept.extend_from_slice(
            format!("\x1b[0m\x1b[1;1H\x1b[L\x1b[7m{text}\x1b[0m\x1b[?25l\x1b[1;1H").as_bytes(),
        );
        if kept.len() > KEPT_BYTES_MAX {
            return;
        }
        if self.kept.len() == KEPT_MAX {
            self.kept.pop_front();
        }
        self.kept.push_back(Kept {
            agent: agent.clone(),
            cols,
            rows,
            screen: kept,
        });
        assert!(self.kept.len() <= KEPT_MAX);
    }

    /// Forgets an agent's kept screen, as its next run starts. Input: the agent. Output: none.
    fn forget(&mut self, agent: &AgentId) {
        self.kept.retain(|kept| kept.agent != *agent);
    }

    /// Opens a finished view of an agent's kept screen: its size, the screen and no output
    /// after it. Input: the agent. Output: the view, or none when nothing is kept.
    fn view(&self, agent: &AgentId) -> Option<TerminalStream> {
        let kept = self.kept.iter().find(|kept| kept.agent == *agent)?;
        let (feed, chunks) = buffer();
        drop(feed);
        Some(TerminalStream {
            agent: kept.agent.clone(),
            cols: kept.cols,
            rows: kept.rows,
            screen: kept.screen.clone(),
            chunks,
        })
    }
}

/// A terminal host that keeps the last screen of each terminal it ends, so a viewer opening
/// an agent between runs sees how its last run finished, labeled with the local time it
/// ended, instead of a refusal; a new run of the agent forgets it. Screens live only in the
/// engine's memory: a terminal that exits on its own, or an engine restart, keeps none.
pub(super) struct LastScreenHost<H> {
    inner: H,
    screens: Mutex<LastScreens>,
}

impl<H: Host> LastScreenHost<H> {
    /// Wraps a platform host. Input: the host. Output: the host with nothing kept yet.
    pub(super) fn new(inner: H) -> Self {
        Self {
            inner,
            screens: Mutex::new(LastScreens::default()),
        }
    }

    fn screens(&self) -> MutexGuard<'_, LastScreens> {
        self.screens.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<H: Host> Host for LastScreenHost<H> {
    fn spawn(&self, agent: &AgentId, command: &InteractiveCommand) -> Result<PaneRef, HostError> {
        let pane = self.inner.spawn(agent, command)?;
        self.screens().forget(agent);
        Ok(pane)
    }

    fn attach(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        match self.inner.attach(agent) {
            Err(problem) if problem.0 == "terminal_agent_unknown" => {
                self.screens().view(agent).ok_or(problem)
            }
            attached => attached,
        }
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        self.inner.input(agent, bytes)
    }

    fn input_live(&self, agent: &AgentId) -> Result<bool, HostError> {
        self.inner.input_live(agent)
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.inner.resize(agent, cols, rows)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        let last = self.inner.attach(agent).ok();
        self.inner.kill(agent)?;
        if let Some(last) = last {
            use crate::tui::clock::{clock, local_offset, now_ms};
            let label = format!(" Finished at {} ", clock(now_ms(), local_offset()));
            self.screens()
                .keep(agent, &last.screen, last.cols, last.rows, &label);
        }
        Ok(())
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        self.inner.alive()
    }

    fn close(&self, keep: &[AgentId]) -> Result<(), HostError> {
        self.inner.close(keep)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn agent(id: usize) -> AgentId {
        AgentId::team(i64::try_from(id).unwrap()).unwrap()
    }

    #[test]
    fn last_screens_keep_the_newest_per_agent_within_their_bounds() {
        let mut screens = LastScreens::default();
        for id in 1..=KEPT_MAX + 1 {
            screens.keep(&agent(id), b"screen", 80, 24, " Finished at 10:42 ");
        }
        assert_eq!(screens.kept.len(), KEPT_MAX);
        assert!(
            screens.view(&agent(1)).is_none(),
            "the oldest screen goes first"
        );
        let mut last = screens.view(&agent(2)).unwrap();
        assert_eq!((last.cols, last.rows), (80, 24));
        assert!(last.screen.starts_with(b"screen"));
        assert!(
            last.screen
                .ends_with(b"\x1b[1;1H\x1b[L\x1b[7m Finished at 10:42 \x1b[0m\x1b[?25l\x1b[1;1H")
        );
        assert!(
            last.chunks.try_recv().is_err(),
            "a finished view takes no output"
        );

        screens.keep(&agent(2), b"again", 2, 2, " Finished at 10:43 ");
        let narrow = screens.view(&agent(2)).unwrap();
        assert!(contains(&narrow.screen, b"\x1b[7m F\x1b[0m"));
        assert_eq!(screens.kept.len(), KEPT_MAX);

        screens.keep(&agent(3), &vec![b'x'; KEPT_BYTES_MAX], 80, 24, " Finished ");
        assert!(
            screens.view(&agent(3)).is_none(),
            "an oversized screen is not kept"
        );
        assert_eq!(screens.kept.len(), KEPT_MAX - 1);
        screens.forget(&agent(2));
        assert!(screens.view(&agent(2)).is_none());
        assert_eq!(screens.kept.len(), KEPT_MAX - 2);
    }

    #[test]
    #[should_panic]
    fn last_screens_invariant_detects_too_many_kept() {
        let mut screens = LastScreens::default();
        for id in 1..=KEPT_MAX + 1 {
            screens.kept.push_back(Kept {
                agent: agent(id),
                cols: 80,
                rows: 24,
                screen: Vec::new(),
            });
        }
        screens.keep(&agent(KEPT_MAX + 2), b"screen", 80, 24, " Finished ");
    }

    fn contains(seen: &[u8], wanted: &[u8]) -> bool {
        seen.windows(wanted.len()).any(|window| window == wanted)
    }
}
