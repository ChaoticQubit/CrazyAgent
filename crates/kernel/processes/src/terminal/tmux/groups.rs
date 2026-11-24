use super::*;

#[cfg(all(test, target_os = "macos"))]
mod close_tests;

impl Tmux {
    /// Closes panes with one tmux command, then ends what they leave in their process groups
    /// (FINDING bridge orphan): tmux only hangs up a closing pane, which a busy Codex
    /// app-server the tools-on bridge started in the background outlives, still working on
    /// the user's account. `kill`, `kill_pane` and the server's close go through here. A
    /// command never sent ends no group; one whose answer failed ends only the groups of the
    /// panes no longer open. Takes the control, the open panes the command closes and the
    /// command. Returns the command's answer at once; a reaper ends the groups.
    pub(super) fn close_panes(
        &self,
        control: &Control,
        panes: &[PaneInfo],
        line: &str,
    ) -> Result<(), HostError> {
        let groups = pane_groups(&self.served(panes));
        let (reply, answer) = sync_channel(1);
        control.send(line, Pending::Reply(reply))?;
        let closed = control.wait(&answer).map(drop);
        let ended = match closed {
            Ok(()) => groups,
            Err(_) => self.closed_groups(panes, groups),
        };
        self.reap(ended);
        closed
    }

    /// Keeps the holds of the panes no longer open after a close whose answer failed, and
    /// drops the rest unended; when the open panes cannot be read, no group is ended. Takes
    /// the panes and their holds. Returns the holds to end.
    fn closed_groups(&self, panes: &[PaneInfo], groups: Vec<Ending>) -> Vec<Ending> {
        let Some(open) = self.open_panes() else {
            return Vec::new();
        };
        let gone: Vec<u32> = panes
            .iter()
            .filter(|pane| !open.iter().any(|id| id.as_slice() == pane.id.as_bytes()))
            .map(|pane| pane.pid)
            .collect();
        groups
            .into_iter()
            .filter(|hold| {
                u32::try_from(hold.pid().as_raw_nonzero().get())
                    .is_ok_and(|pid| gone.contains(&pid))
            })
            .collect()
    }

    /// The ids of the panes the server holds open: none once it no longer listens, else as a
    /// control that cannot start a server lists them, asked again every 10 ms while an ending
    /// server drops it, for at most `REPLY_TIME`. Output: the ids, or none when unread.
    fn open_panes(&self) -> Option<Vec<Vec<u8>>> {
        let deadline = Instant::now() + REPLY_TIME;
        loop {
            if !self.running() || self.listening() == Ok(false) {
                return Some(Vec::new());
            }
            let listed = self.control_with(false).and_then(|control| {
                control.ask(&format!("list-panes -a -F '#{{pane_id}}{FORMAT_END}'"))
            });
            if let Ok(open) = listed {
                return Some(open);
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// Ends groups on a thread of their own, so a caller such as the team tick never waits
    /// out the hangup grace and stops in a row share one grace; `close` waits for every
    /// reaper. Past `REAPERS_MAX`, or when no thread starts, the caller waits instead.
    /// Takes the holds on the groups, which the reaper gets once its thread runs.
    pub(super) fn reap(&self, groups: Vec<Ending>) {
        if groups.is_empty() {
            return;
        }
        let Ok(mut reapers) = self.reapers.lock() else {
            end_groups(groups);
            return;
        };
        reapers.retain(|reaper| !reaper.is_finished());
        if reapers.len() >= REAPERS_MAX {
            let _ = reapers.remove(0).join();
        }
        let (hand, take) = std::sync::mpsc::channel::<Vec<Ending>>();
        match std::thread::Builder::new()
            .name("tmux-reaper".into())
            .spawn(move || {
                if let Ok(groups) = take.recv() {
                    end_groups(groups);
                }
            }) {
            Ok(reaper) => {
                reapers.push(reaper);
                if let Err(std::sync::mpsc::SendError(groups)) = hand.send(groups) {
                    end_groups(groups);
                }
            }
            Err(_) => end_groups(groups),
        }
        assert!(reapers.len() <= REAPERS_MAX);
    }

    /// Waits until every reaper this host started has ended its groups. Takes the host.
    pub(crate) fn join_reapers(&self) {
        let reapers = self
            .reapers
            .lock()
            .map(|mut reapers| std::mem::take(&mut *reapers))
            .unwrap_or_default();
        for reaper in reapers {
            let _ = reaper.join();
        }
    }

    /// Ends every terminal not in `keep` and, with none kept, the server itself, unless the
    /// listing found it ended, as a lead's own server its keeper ended; `close` runs it and
    /// then waits for the reapers. Takes the names to keep. Returns the first failure.
    pub(super) fn end_server(&self, keep: &[AgentId]) -> Result<(), HostError> {
        if self.executable.is_none() || !self.running() {
            return Ok(());
        }
        end_except(self, keep)?;
        if !keep.is_empty() || !self.running() {
            return Ok(());
        }
        let control = self.control_with(self.confined.is_none())?;
        let panes: Vec<PaneInfo> = control
            .ask(&format!("list-panes -a -F '{PANE_FIELDS}{FORMAT_END}'"))
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| pane_info(line))
                    .take(PANES_LISTED_MAX)
                    .collect()
            })
            .unwrap_or_default();
        let _ = self.close_panes(&control, &panes, "kill-server");
        control.close();
        if let Ok(mut slot) = self.control.lock() {
            *slot = None;
        }
        self.forget_server()
    }
}

/// The process groups of open panes, read before they close: tmux starts each pane's first
/// process as the leader of its own session and group, where the processes it starts in the
/// background stay. A pane whose first process ended or leads no group of its own gives
/// none, so a stop never signals a group it did not read from its pane. A group another
/// path holds, as the session stop that ends this pane, is left to that path, so one group
/// is signalled from one path only. Takes the panes. Returns the holds on
/// their groups.
pub(super) fn pane_groups(panes: &[PaneInfo]) -> Vec<Ending> {
    assert!(panes.len() <= PANES_LISTED_MAX);
    let own = rustix::process::getpgrp();
    panes
        .iter()
        .filter(|pane| !pane.dead)
        .filter_map(|pane| {
            let leader = rustix::process::Pid::from_raw(i32::try_from(pane.pid).ok()?)?;
            let leads = leader.as_raw_nonzero().get() > 1
                && leader != own
                && rustix::process::getpgid(Some(leader)) == Ok(leader);
            if leads { Ending::claim(pane.pid) } else { None }
        })
        .collect()
}

/// Waits for the groups of closed panes to end on the hangup, for at most `HANGUP_GRACE`,
/// then kills what is left until no group answers, for at most `GROUP_KILL_WAIT`. macOS
/// answers EPERM for a group whose last member it just reaped, so only ESRCH ends a group,
/// and a group that answered ESRCH once is never tested or signalled again: its number may
/// lead a new group by then; its hold is marked gone and dropped then, so the
/// number stays refused to later paths. Takes the holds `pane_groups` took
/// before their panes closed.
pub(super) fn end_groups(groups: Vec<Ending>) {
    end_groups_with(
        groups,
        |group| rustix::process::test_kill_process_group(group) != Err(rustix::io::Errno::SRCH),
        |group| {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        },
    );
}

/// Ends the groups as `end_groups` describes, through `left`, which tells whether a group
/// still answers, and `kill`, which sends it KILL. Takes the holds and both calls.
pub(super) fn end_groups_with(
    groups: Vec<Ending>,
    mut left: impl FnMut(rustix::process::Pid) -> bool,
    mut kill: impl FnMut(rustix::process::Pid),
) {
    let mut groups = groups;
    let hung_up = Instant::now() + HANGUP_GRACE;
    loop {
        keep_left(&mut groups, &mut left);
        if groups.is_empty() || Instant::now() >= hung_up {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let killed = Instant::now() + GROUP_KILL_WAIT;
    while !groups.is_empty() && Instant::now() < killed {
        for group in &groups {
            kill(group.pid());
        }
        std::thread::sleep(Duration::from_millis(10));
        keep_left(&mut groups, &mut left);
    }
}

/// Keeps the holds whose group still answers `left` and marks every other one gone, so
/// its number stays refused to later paths once dropped. Takes the holds and
/// the test.
pub(super) fn keep_left(
    groups: &mut Vec<Ending>,
    left: &mut impl FnMut(rustix::process::Pid) -> bool,
) {
    groups.retain_mut(|group| {
        let answers = left(group.pid());
        if !answers {
            group.gone();
        }
        answers
    });
}
