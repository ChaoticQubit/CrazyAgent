use crate::restore::Recovery;
use crate::tui::screens::settings::restore::RESTORE_RUNNING;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

/// What the recovery screen says when a restore did not finish, in the desktop's words.
pub const RECOVERY_BROKEN: &str = "Restore did not finish. Your data is safe in a backup.";
/// What the recovery screen says when a restore stopped before it changed anything.
pub const RECOVERY_CONTINUE: &str =
    "Restore stopped before it changed anything. Your 2.5 data is unchanged.";
/// The action that puts the copy the restore record names back over the data.
pub const PUT_BACK: &str = "Put the backup back";
/// The action that carries on with the unchanged 2.5 data.
pub const CONTINUE: &str = "Continue with your 2.5 data";

/// The recovery offer for the TUI's own engine and the backups folder it names.
#[derive(Debug, PartialEq, Eq)]
pub struct Read {
    pub offer: Option<Recovery>,
    pub folder: PathBuf,
}

/// One recovery job, run away from the screen's thread since each checks engine.db.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    Read,
    PutBack,
    Continue,
    Finish,
}

/// How a job ended: a read, or an action with whether a start may follow.
enum Done {
    Read(Result<Read, String>),
    Acted(Step, Result<(), String>),
}

/// What a finished job asks of the screen: nothing, a start of the engine, or a reconnect
/// that only reads how the engine stands.
#[derive(Debug, PartialEq, Eq)]
pub enum Next {
    Stay,
    Start,
    Reconnect,
}

/// The engine a recovery job acts on: the layout and login service the TUI started with.
pub type Engine = (crate::paths::Layout, kernel_processes::service::Service);

/// The recovery screen's state: the engine it acts on, the last read, the last refusal and
/// the job in flight.
#[derive(Default)]
pub struct Recovering {
    engine: Option<Engine>,
    read: Option<Result<Read, String>>,
    problem: Option<String>,
    pending: Option<Receiver<Done>>,
}

impl Recovering {
    /// Whether a job runs. Output: the answer.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Starts the first read, once, on the engine the TUI started with. Input: that engine,
    /// none when the TUI starts no engine. Output: none.
    pub fn start(&mut self, engine: Option<&Engine>) {
        if self.engine.is_none() {
            self.engine = engine.cloned();
        }
        if self.read.is_none() && self.pending.is_none() {
            self.spawn(Step::Read);
        }
    }

    /// A screen whose read has just come back, as the poll finds it. Input: the read.
    /// Output: the screen.
    #[cfg(test)]
    pub(crate) fn answered(read: Result<Read, String>) -> Self {
        let (send, receive) = channel();
        send.send(Done::Read(read)).unwrap();
        Self {
            pending: Some(receive),
            ..Self::default()
        }
    }

    /// A screen showing `read` whose action has just ended, as the poll finds it. Inputs: the
    /// read, the action and how it ended. Output: the screen.
    #[cfg(test)]
    fn acted(read: Result<Read, String>, step: Step, result: Result<(), String>) -> Self {
        let (send, receive) = channel();
        send.send(Done::Acted(step, result)).unwrap();
        Self {
            read: Some(read),
            pending: Some(receive),
            ..Self::default()
        }
    }

    /// Takes a finished job: a read that found a restore whose 2.4 data is written runs the
    /// finish, a read with nothing to recover reconnects to show how the engine stands, a
    /// put-back or continue that ended well asks for the engine, and one refused keeps its
    /// code and reads again. Output: what the screen does next.
    pub fn poll(&mut self) -> Next {
        let Some(receive) = &self.pending else {
            return Next::Stay;
        };
        let done = match receive.try_recv() {
            Ok(done) => done,
            Err(TryRecvError::Empty) => return Next::Stay,
            Err(TryRecvError::Disconnected) => Done::Read(Err("storage_restore_failed".into())),
        };
        self.pending = None;
        match done {
            Done::Read(Ok(Read { offer: None, .. })) => Next::Reconnect,
            Done::Read(read) => {
                let finished = matches!(
                    read,
                    Ok(Read {
                        offer: Some(Recovery::Finished { .. }),
                        ..
                    })
                );
                self.read = Some(read);
                if finished {
                    self.spawn(Step::Finish);
                }
                Next::Stay
            }
            Done::Acted(step, Ok(())) => {
                self.problem = None;
                match step {
                    Step::Finish => Next::Reconnect,
                    _ => Next::Start,
                }
            }
            Done::Acted(step, Err(code)) => {
                self.problem = Some(code);
                if step != Step::Finish {
                    self.spawn(Step::Read);
                }
                Next::Stay
            }
        }
    }

    /// Runs the action of a key the read offers, or `r` to read again. Input: the key.
    /// Output: whether a job started.
    pub fn key(&mut self, key: char) -> bool {
        if self.busy() {
            return false;
        }
        let step = match key {
            'r' => Step::Read,
            'p' if self.offers('p') => Step::PutBack,
            'n' if self.offers('n') => Step::Continue,
            _ => return false,
        };
        self.problem = None;
        self.spawn(step);
        true
    }

    /// The screen's lines for the last read, or the sentence alone before one. Output: them.
    pub fn lines(&self) -> Vec<String> {
        match &self.read {
            Some(read) => lines(read, self.problem.as_deref()),
            None => vec![RECOVERY_BROKEN.to_owned()],
        }
    }

    /// The keys the last read offers, with their labels. Output: them.
    pub fn actions(&self) -> Vec<(char, &'static str)> {
        self.read.as_ref().map(actions).unwrap_or_default()
    }

    /// Whether the last read offers a key. Input: the key. Output: the answer.
    fn offers(&self, key: char) -> bool {
        self.actions().iter().any(|(offered, _)| *offered == key)
    }

    /// Starts one job on its own thread. Input: the step. Output: none.
    fn spawn(&mut self, step: Step) {
        assert!(self.pending.is_none(), "one recovery job at a time");
        let (send, receive) = channel();
        let engine = self.engine.clone();
        std::thread::spawn(move || {
            let _ = send.send(job(step, engine));
        });
        self.pending = Some(receive);
    }
}

/// The lines a read shows: the sentence for its offer, the backups folder unless the data is
/// unchanged, and the refusal's code when there is one. Inputs: the read and the last
/// action's refusal. Output: the lines.
pub fn lines(read: &Result<Read, String>, problem: Option<&str>) -> Vec<String> {
    let mut lines = match read {
        Ok(Read {
            offer: Some(Recovery::Continue),
            ..
        }) => vec![RECOVERY_CONTINUE.to_owned()],
        Ok(Read {
            offer: Some(Recovery::Finished { .. }),
            ..
        }) => vec![RESTORE_RUNNING.to_owned()],
        Ok(Read {
            offer: Some(Recovery::Stuck { code }),
            folder,
        }) => vec![
            RECOVERY_BROKEN.to_owned(),
            format!("Backups folder: {}", folder.display()),
            format!("Code: {code}"),
        ],
        Ok(Read { folder, .. }) => vec![
            RECOVERY_BROKEN.to_owned(),
            format!("Backups folder: {}", folder.display()),
        ],
        Err(code) => vec![RECOVERY_BROKEN.to_owned(), format!("Code: {code}")],
    };
    lines.extend(problem.map(|code| format!("Code: {code}")));
    assert!(!lines.is_empty());
    lines
}

/// The keys a read offers with their labels: put-back for a named copy that passed its
/// check, continue for data a restore left unchanged, none otherwise. Input: the read.
/// Output: the keys.
pub fn actions(read: &Result<Read, String>) -> Vec<(char, &'static str)> {
    match read {
        Ok(Read {
            offer: Some(Recovery::PutBack { .. }),
            ..
        }) => vec![('p', PUT_BACK)],
        Ok(Read {
            offer: Some(Recovery::Continue),
            ..
        }) => vec![('n', CONTINUE)],
        _ => Vec::new(),
    }
}

/// Runs one job on the engine the TUI started with, weighing whether its login service still
/// runs a process. Inputs: the step and that engine. Output: how it ended, `not_configured`
/// without an engine.
fn job(step: Step, engine: Option<Engine>) -> Done {
    match engine {
        Some((layout, service)) => run(step, &layout, crate::restore::service_running(&service)),
        None if step == Step::Read => Done::Read(Err("not_configured".into())),
        None => Done::Acted(step, Err("not_configured".into())),
    }
}

/// Runs one job on an engine's root, refusing while its login service still runs a process.
/// Inputs: the step, the engine's layout and whether its service runs. Output: how it ended,
/// a refusal as its code.
fn run(step: Step, layout: &crate::paths::Layout, running: bool) -> Done {
    let code = |text: String| crate::restore::recovery_code(&text).to_owned();
    match step {
        Step::Read => Done::Read(
            crate::restore::recovery(layout.root(), running)
                .map(|offer| Read {
                    offer,
                    folder: crate::restore::backups(layout.root()),
                })
                .map_err(code),
        ),
        Step::PutBack => Done::Acted(
            step,
            crate::restore::put_back(layout.root(), running).map_err(code),
        ),
        Step::Continue => Done::Acted(
            step,
            crate::restore::continue_2_5(layout.root(), running).map_err(code),
        ),
        Step::Finish => Done::Acted(
            step,
            crate::restore::finish(
                layout.root(),
                running,
                &layout.global_settings(),
                &layout.project_settings(),
            )
            .map_err(code),
        ),
    }
}

#[cfg(test)]
mod tests;
