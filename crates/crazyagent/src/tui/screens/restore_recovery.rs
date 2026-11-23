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

/// The recovery screen's state: the last read, the last refusal and the job in flight.
#[derive(Default)]
pub struct Recovering {
    read: Option<Result<Read, String>>,
    problem: Option<String>,
    pending: Option<Receiver<Done>>,
}

impl Recovering {
    /// Whether a job runs. Output: the answer.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Starts the first read, once. Output: none.
    pub fn start(&mut self) {
        if self.read.is_none() && self.pending.is_none() {
            self.spawn(Step::Read);
        }
    }

    /// Takes a finished job: a read that found a restore whose 2.4 data is written runs the
    /// finish, and a read with nothing to recover or an action that ended well asks for the
    /// engine. Output: what the screen does next.
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
            Done::Read(Ok(Read { offer: None, .. })) => Next::Start,
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
            Done::Acted(_, Err(code)) => {
                self.problem = Some(code);
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
        std::thread::spawn(move || {
            let _ = send.send(run(step));
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

/// Runs one job on the default engine's root, refusing while its login service still runs a
/// process. Input: the step. Output: how it ended, a refusal as its code.
fn run(step: Step) -> Done {
    let engine = engine();
    let code = |text: String| crate::restore::recovery_code(&text).to_owned();
    match (step, engine) {
        (Step::Read, Ok((layout, running))) => Done::Read(
            crate::restore::recovery(layout.root(), running)
                .map(|offer| Read {
                    offer,
                    folder: crate::restore::backups(layout.root()),
                })
                .map_err(code),
        ),
        (Step::Read, Err(problem)) => Done::Read(Err(problem)),
        (step, Err(problem)) => Done::Acted(step, Err(problem)),
        (Step::PutBack, Ok((layout, running))) => Done::Acted(
            step,
            crate::restore::put_back(layout.root(), running).map_err(code),
        ),
        (Step::Continue, Ok((layout, running))) => Done::Acted(
            step,
            crate::restore::continue_2_5(layout.root(), running).map_err(code),
        ),
        (Step::Finish, Ok((layout, running))) => Done::Acted(
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

/// The default engine's layout and whether its login service still runs a process.
/// Output: them, or `not_configured` when there is no default engine.
fn engine() -> Result<(crate::paths::Layout, bool), String> {
    let layout = crate::paths::Layout::current().map_err(|_| "not_configured".to_owned())?;
    let executable = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .map_err(|_| "not_configured".to_owned())?;
    let service =
        crate::service::current_user(kernel_common::Platform::CURRENT, executable, &layout)
            .map_err(|_| "not_configured".to_owned())?;
    let running = crate::restore::service_running(&service);
    Ok((layout, running))
}

#[cfg(test)]
mod tests;
