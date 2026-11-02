#![allow(dead_code)]

use crazyagent::team::caller::{Callers, Whom, socket_for_mailbox};
use crazyagent::team::tool::MAILBOX_FOLDER;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// The pane a lead and its in-process teammates run in, which names no teammate.
pub const LEAD_PANE: &str = "%1";

/// The engine's side of a native lead's caller socket for relay tests: the real `Callers`
/// bound where the lead's mailbox finds it and served on a thread, naming each process by
/// the pane the test started it in, as the engine names it by the panes tmux lists, and
/// answering `who` with the teammate the test registered for that pane, or the lead for
/// any other pane, as the engine answers from its rows. The real tmux chain and the
/// engine's own rows are proven in `coord_hook_pane` and the runtime tests.
pub struct PaneCallers {
    panes: Arc<Mutex<Vec<(String, u32)>>>,
    stop: Arc<AtomicBool>,
    serving: Option<JoinHandle<()>>,
    socket: PathBuf,
}

impl PaneCallers {
    /// Makes a lead's mailbox in the engine's layout below `root`, private, and binds and
    /// serves its caller socket. Takes the root and the teammates as (pane, name). Returns
    /// the mailbox and the served socket.
    pub fn new(root: &Path, teammates: &[(&str, &str)]) -> (PathBuf, Self) {
        let mailbox = root.join("agent-1").join("tmp").join(MAILBOX_FOLDER);
        std::fs::create_dir_all(&mailbox).unwrap();
        std::fs::set_permissions(&mailbox, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = socket_for_mailbox(&mailbox).unwrap();
        let short = socket.parent().unwrap();
        assert!(!short.exists(), "{}", short.display());
        std::fs::create_dir(short).unwrap();
        std::fs::set_permissions(short, std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut callers = Callers::bind(&socket, None).unwrap();
        let panes: Arc<Mutex<Vec<(String, u32)>>> = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let names: Vec<(String, String)> = teammates
            .iter()
            .map(|(pane, name)| ((*pane).to_owned(), (*name).to_owned()))
            .collect();
        let serving = {
            let panes = Arc::clone(&panes);
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    callers.serve(&mut || panes.lock().unwrap().clone(), &mut |pane| {
                        Some(
                            names
                                .iter()
                                .find(|(listed, _)| listed == pane)
                                .map_or(Whom::Lead, |(_, name)| Whom::Teammate(name.clone())),
                        )
                    });
                    std::thread::sleep(Duration::from_millis(10));
                }
            })
        };
        (
            mailbox,
            Self {
                panes,
                stop,
                serving: Some(serving),
                socket,
            },
        )
    }

    /// Starts a process in `pane`: it is listed there before the engine can answer it, as
    /// tmux lists a pane's process. Takes the pane and the command. Returns the child.
    pub fn spawn(&self, pane: &str, command: &mut Command) -> Child {
        let mut panes = self.panes.lock().unwrap();
        let child = crazyagent::spawn_gate::spawn(command).unwrap();
        panes.push((pane.to_owned(), child.id()));
        child
    }
}

impl Drop for PaneCallers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(serving) = self.serving.take() {
            let _ = serving.join();
        }
        let _ = std::fs::remove_file(&self.socket);
        if let Some(folder) = self.socket.parent() {
            let _ = std::fs::remove_dir(folder);
        }
    }
}
