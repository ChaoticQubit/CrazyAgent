use super::buffer::buffer;
use super::{AgentId, Host, HostError, PaneInfo, PaneRef, TerminalStream, VIEWER_BUFFER_BYTES_MAX};
use crate::harnesses::interactive::InteractiveCommand;
use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard, PoisonError};

/// How many ended terminals keep their last screen: two full projects of agents. Past it the
/// screen kept longest ago is forgotten first.
pub(crate) const KEPT_MAX: usize = 32;
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

    /// Tells whether an agent's last screen is kept. Input: the agent. Output: the answer.
    fn holds(&self, agent: &AgentId) -> bool {
        self.kept.iter().any(|kept| kept.agent == *agent)
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
            history: Vec::new(),
            screen: kept.screen.clone(),
            chunks,
        })
    }
}

/// The label over a kept screen: the local time its terminal ended. Output: the label.
fn finished_label() -> String {
    use crate::tui::clock::{clock, local_offset, now_ms};
    format!(" Finished at {} ", clock(now_ms(), local_offset()))
}

/// A terminal host that keeps the last screen of each terminal it ends, so a viewer opening
/// an agent between runs sees how its last run finished, labeled with the local time it
/// ended, instead of a refusal; a new run of the agent forgets it. A kill keeps no newer
/// screen over one already kept for the run, as before a keeper ends it. A kill and a spawn
/// each hold the kept screens while they act, so a respawn never lands between a kill and
/// the screen it keeps. Screens live only in the engine's memory: a terminal that exits on its
/// own, or an engine restart, keeps none.
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
        let mut screens = self.screens();
        let pane = self.inner.spawn(agent, command)?;
        screens.forget(agent);
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

    fn screen(&self, agent: &AgentId) -> Result<Vec<u8>, HostError> {
        self.inner.screen(agent)
    }

    fn kept(&self, agent: &AgentId) -> bool {
        self.screens().holds(agent)
    }

    fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
        self.inner.snapshot(agent)
    }

    fn keep_last(&self, agent: &AgentId) {
        let mut screens = self.screens();
        if let Ok(last) = self.inner.snapshot(agent) {
            screens.keep(agent, &last.screen, last.cols, last.rows, &finished_label());
        }
    }

    fn input(&self, agent: &AgentId, bytes: &[u8]) -> Result<(), HostError> {
        self.inner.input(agent, bytes)
    }

    fn input_live(&self, agent: &AgentId) -> Result<bool, HostError> {
        self.inner.input_live(agent)
    }

    fn quiet(&self, agent: &AgentId) -> Result<Option<std::time::Duration>, HostError> {
        self.inner.quiet(agent)
    }

    fn resize(&self, agent: &AgentId, cols: u16, rows: u16) -> Result<(), HostError> {
        self.inner.resize(agent, cols, rows)
    }

    fn kill(&self, agent: &AgentId) -> Result<(), HostError> {
        let mut screens = self.screens();
        let last = if screens.holds(agent) {
            None
        } else {
            self.inner.snapshot(agent).ok()
        };
        self.inner.kill(agent)?;
        if let Some(last) = last {
            screens.keep(agent, &last.screen, last.cols, last.rows, &finished_label());
        }
        Ok(())
    }

    fn panes(&self, agent: &AgentId) -> Result<Vec<PaneInfo>, HostError> {
        self.inner.panes(agent)
    }

    fn kill_pane(&self, agent: &AgentId, pane: &str) -> Result<(), HostError> {
        self.inner.kill_pane(agent, pane)
    }

    fn attach_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.inner.attach_pane(agent, pane)
    }

    fn snapshot_pane(&self, agent: &AgentId, pane: &str) -> Result<TerminalStream, HostError> {
        self.inner.snapshot_pane(agent, pane)
    }

    fn input_pane(&self, agent: &AgentId, pane: &str, bytes: &[u8]) -> Result<(), HostError> {
        self.inner.input_pane(agent, pane, bytes)
    }

    fn input_live_pane(&self, agent: &AgentId, pane: &str) -> Result<bool, HostError> {
        self.inner.input_live_pane(agent, pane)
    }

    fn adopt_pane(&self, agent: &AgentId, lead: &AgentId, pane: &str) -> Result<(), HostError> {
        self.inner.adopt_pane(agent, lead, pane)
    }

    fn spawn_own_server(
        &self,
        agent: &AgentId,
        command: &InteractiveCommand,
    ) -> Result<PaneRef, HostError> {
        let mut screens = self.screens();
        let pane = self.inner.spawn_own_server(agent, command)?;
        screens.forget(agent);
        Ok(pane)
    }
    fn pane_pid(&self, agent: &AgentId) -> Result<Option<u32>, HostError> {
        self.inner.pane_pid(agent)
    }

    fn unkept(&self, agent: &AgentId) -> bool {
        self.inner.unkept(agent)
    }

    fn end_kept_server(&self, agent: &AgentId) -> Option<bool> {
        self.inner.end_kept_server(agent)
    }

    fn server_keepers(&self, agent: &AgentId) -> Vec<(u32, bool)> {
        self.inner.server_keepers(agent)
    }

    fn alive(&self) -> Result<Vec<AgentId>, HostError> {
        self.inner.alive()
    }

    fn close(&self, keep: &[AgentId]) -> Result<(), HostError> {
        self.inner.close(keep)
    }

    fn renew(&self, until: std::time::SystemTime) -> Result<(), HostError> {
        self.inner.renew(until)
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

    /// A host whose snapshot of one agent waits until the test lets it go, as a tmux server
    /// its native lead stopped answers no capture; every other call answers at once.
    struct Stalled {
        stuck: AgentId,
        entered: Mutex<std::sync::mpsc::Sender<()>>,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }

    impl Host for Stalled {
        fn spawn(&self, agent: &AgentId, _: &InteractiveCommand) -> Result<PaneRef, HostError> {
            Ok(PaneRef {
                agent: agent.clone(),
                pane: "%1".into(),
                pid: 1,
            })
        }

        fn attach(&self, _: &AgentId) -> Result<TerminalStream, HostError> {
            Err(HostError("terminal_agent_unknown".into()))
        }

        fn snapshot(&self, agent: &AgentId) -> Result<TerminalStream, HostError> {
            if *agent == self.stuck {
                let _ = self.entered.lock().unwrap().send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap()
                    .recv_timeout(std::time::Duration::from_secs(30));
            }
            Err(HostError("terminal_capture_failed".into()))
        }

        fn input(&self, _: &AgentId, _: &[u8]) -> Result<(), HostError> {
            Ok(())
        }

        fn resize(&self, _: &AgentId, _: u16, _: u16) -> Result<(), HostError> {
            Ok(())
        }

        fn kill(&self, _: &AgentId) -> Result<(), HostError> {
            Ok(())
        }

        fn alive(&self) -> Result<Vec<AgentId>, HostError> {
            Ok(Vec::new())
        }
    }

    /// One agent's kill waiting on a stalled tmux server holds up only that agent: another
    /// agent's spawn and kill and a read of the kept screens answer while it waits.
    #[test]
    fn a_stalled_kill_holds_up_no_other_agents_spawn_kill_or_kept_read() {
        use std::sync::{Arc, mpsc};
        use std::time::Duration;
        let (entered_tx, entered) = mpsc::channel();
        let (release, release_rx) = mpsc::channel();
        let host = Arc::new(LastScreenHost::new(Stalled {
            stuck: agent(1),
            entered: Mutex::new(entered_tx),
            release: Mutex::new(release_rx),
        }));
        let killer = {
            let host = Arc::clone(&host);
            std::thread::spawn(move || host.kill(&agent(1)))
        };
        entered
            .recv_timeout(Duration::from_secs(10))
            .expect("the kill reached the stalled snapshot");
        let (done, answers) = mpsc::channel();
        {
            let host = Arc::clone(&host);
            std::thread::spawn(move || {
                let command = InteractiveCommand {
                    argv: vec!["/bin/true".into()],
                    cwd: std::env::temp_dir(),
                    env: Vec::new(),
                    transcript: crate::harnesses::interactive::Transcript::Unavailable,
                };
                let spawned = host.spawn(&agent(2), &command).is_ok();
                let killed = host.kill(&agent(3)).is_ok();
                let kept = host.kept(&agent(1));
                let _ = done.send((spawned, killed, kept));
            });
        }
        let answered = answers.recv_timeout(Duration::from_secs(5));
        release.send(()).unwrap();
        killer.join().unwrap().unwrap();
        assert_eq!(
            answered,
            Ok((true, true, false)),
            "another agent waited on the stalled kill"
        );
    }

    #[cfg(unix)]
    struct Server(std::path::PathBuf, std::path::PathBuf);

    #[cfg(unix)]
    impl Drop for Server {
        fn drop(&mut self) {
            let _ = crate::spawn_gate::status(
                std::process::Command::new(&self.0)
                    .arg("-S")
                    .arg(&self.1)
                    .arg("kill-server")
                    .env_clear()
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null()),
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_respawn_never_slips_between_a_kill_and_the_screen_it_keeps() {
        use crate::terminal::tmux::{SOCKET, Tmux, installed};
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;
        use std::time::Duration;

        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("terminal");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let program = installed(home.path()).expect("tmux is required for this test");
        let _server = Server(program.clone(), folder.join(SOCKET));
        let host = Arc::new(LastScreenHost::new(Tmux::open(Some(program), &folder)));
        let named = agent(9);
        let run = |text: &str| InteractiveCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("printf '{text}\\n'; exec /bin/sleep 30"),
            ],
            cwd: home.path().to_path_buf(),
            env: Vec::new(),
            transcript: crate::harnesses::interactive::Transcript::Unavailable,
        };
        host.spawn(&named, &run("first run")).unwrap();
        assert_eq!(host.alive().unwrap(), vec![named.clone()]);

        let held = host.screens();
        let killing = {
            let (host, named) = (Arc::clone(&host), named.clone());
            std::thread::spawn(move || host.kill(&named))
        };
        std::thread::sleep(Duration::from_millis(500));
        assert_eq!(
            host.alive().unwrap(),
            vec![named.clone()],
            "a kill ends the terminal only while it holds the kept screens"
        );
        drop(held);
        killing.join().unwrap().unwrap();
        assert!(host.alive().unwrap().is_empty());

        let held = host.screens();
        assert!(held.view(&named).is_some());
        let spawning = {
            let (host, named) = (Arc::clone(&host), named.clone());
            let second = run("second run");
            std::thread::spawn(move || host.spawn(&named, &second).map(drop))
        };
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            host.alive().unwrap().is_empty(),
            "a respawn starts the terminal only while it holds the kept screens"
        );
        drop(held);
        spawning.join().unwrap().unwrap();
        assert_eq!(host.alive().unwrap(), vec![named.clone()]);
        assert!(host.screens().view(&named).is_none());
        host.kill(&named).unwrap();
    }

    /// The host says it keeps an agent's screen only between that agent's kill and its next
    /// run, never for a live terminal or an agent that never ran (N-D21).
    #[cfg(unix)]
    #[test]
    fn a_host_keeps_a_screen_only_between_a_kill_and_the_next_run() {
        use crate::terminal::tmux::{SOCKET, Tmux, installed};
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("terminal");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let program = installed(home.path()).expect("tmux is required for this test");
        let _server = Server(program.clone(), folder.join(SOCKET));
        let host = LastScreenHost::new(Tmux::open(Some(program), &folder));
        let (named, never) = (agent(9), agent(10));
        let run = |text: &str| InteractiveCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                format!("printf '{text}\\n'; exec /bin/sleep 30"),
            ],
            cwd: home.path().to_path_buf(),
            env: Vec::new(),
            transcript: crate::harnesses::interactive::Transcript::Unavailable,
        };
        assert!(!host.kept(&named), "nothing is kept before a run");
        host.spawn(&named, &run("first run")).unwrap();
        assert!(!host.kept(&named), "a live terminal keeps no last screen");
        host.kill(&named).unwrap();
        assert!(host.kept(&named), "an ended terminal keeps its last screen");
        assert!(!host.kept(&never), "an agent that never ran keeps nothing");
        host.spawn(&named, &run("second run")).unwrap();
        assert!(!host.kept(&named), "a new run forgets the screen kept");
        host.kill(&named).unwrap();
        assert!(host.kept(&named));
    }

    /// KEPT-L1: when a terminal outlives the stop that kept its last screen, as when its
    /// keeper does not end it in time, and its screen then changes, as when the harness clears
    /// it on the way out, the kill that follows keeps the screen kept before, not the cleared
    /// one.
    #[cfg(unix)]
    #[test]
    fn a_kill_after_the_last_screen_was_kept_never_replaces_it_with_a_later_screen() {
        use crate::terminal::tmux::{SOCKET, Tmux, installed};
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let home = tempfile::tempdir().unwrap();
        let folder = home.path().join("terminal");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let program = installed(home.path()).expect("tmux is required for this test");
        let _server = Server(program.clone(), folder.join(SOCKET));
        let host = LastScreenHost::new(Tmux::open(Some(program), &folder));
        let named = agent(11);
        let run = InteractiveCommand {
            argv: vec![
                "/bin/sh".into(),
                "-c".into(),
                "printf 'harness answer 3825\\n'; read line; printf '\\033[2J\\033[H'; exec /bin/sleep 30"
                    .into(),
            ],
            cwd: home.path().to_path_buf(),
            env: Vec::new(),
            transcript: crate::harnesses::interactive::Transcript::Unavailable,
        };
        let shows = |wanted: bool| {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let screen = host.snapshot(&named).unwrap().screen;
                if contains(&screen, b"harness answer 3825") == wanted {
                    return true;
                }
                if Instant::now() >= deadline {
                    return false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        };
        host.spawn(&named, &run).unwrap();
        assert!(shows(true), "the terminal never showed its answer");
        host.keep_last(&named);
        assert!(host.kept(&named));
        host.input(&named, b"\r").unwrap();
        assert!(shows(false), "the terminal never cleared its screen");

        host.kill(&named).unwrap();
        assert!(host.alive().unwrap().is_empty());
        let finished = host.attach(&named).unwrap();
        assert!(
            contains(&finished.screen, b"harness answer 3825"),
            "the kill replaced the kept last screen with the cleared one: {}",
            String::from_utf8_lossy(&finished.screen)
        );
        assert!(contains(&finished.screen, b" Finished at "));
    }
}
