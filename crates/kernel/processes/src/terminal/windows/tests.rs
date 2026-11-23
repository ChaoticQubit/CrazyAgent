use super::*;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;

/// The bytes the stalled writer holds on, as a pane whose app stopped reading its input
/// holds a write to it.
const STALLED: &[u8] = b"stalled input";

/// How long a call on the host may take while one pane's write is stalled.
const ANSWER_MAX: Duration = Duration::from_secs(10);

/// A pane's input that passes every write on to the pane's own writer except the stalled
/// bytes: for those it reports that the write began, then blocks until released, as a full
/// ConPTY input pipe blocks a write. Fields: the pane's writer and the two channel ends.
struct Stall {
    inner: Box<dyn Write + Send>,
    entered: Sender<()>,
    release: Receiver<()>,
}

impl Write for Stall {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes != STALLED {
            return self.inner.write(bytes);
        }
        let _ = self.entered.send(());
        let _ = self.release.recv();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// A pane command that keeps a cmd.exe prompt open. Input: the folder it starts in.
/// Output: the command.
fn shell(cwd: &Path) -> PaneCommand {
    let root = std::env::var_os("SystemRoot").map(PathBuf::from).unwrap();
    PaneCommand {
        argv: vec![
            root.join("System32").join("cmd.exe").display().to_string(),
            "/Q".into(),
            "/K".into(),
        ],
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
    }
}

/// An app that stops reading its input leaves a write to its pane blocked. While it is, the
/// host still lists its live terminals, and resizes and types into every other pane; the
/// stalled write ends once the pane takes its input again.
#[test]
fn a_write_stalled_on_one_pane_leaves_every_other_call_answering() {
    let temp = tempfile::tempdir().unwrap();
    let host = ConptyHost::new(|_, _| true);
    let stalled = AgentId::team(1).unwrap();
    let other = AgentId::team(2).unwrap();
    for agent in [&stalled, &other] {
        host.spawn(agent, &shell(temp.path())).unwrap();
    }
    let (entered, began) = channel();
    let (release, released) = channel();
    let writer = Arc::clone(&lock(&host.panes)[&stalled].writer);
    {
        let mut held = lock(&writer);
        let inner = std::mem::replace(&mut *held, Box::new(std::io::sink()));
        *held = Box::new(Stall {
            inner,
            entered,
            release: released,
        });
    }
    let mut want = vec![stalled.clone(), other.clone()];
    want.sort();
    let (host, stalled, other) = (&host, &stalled, &other);
    std::thread::scope(|scope| {
        let writing = scope.spawn(move || host.input(stalled, STALLED));
        began
            .recv_timeout(ANSWER_MAX)
            .expect("the stalled write began");
        let (answer, answers) = channel();
        scope.spawn(move || {
            let alive = host.alive();
            let resized = host.resize(other, 100, 30);
            let typed = host.input(other, b"rem\r");
            let _ = answer.send((alive, resized, typed));
        });
        let answered = answers.recv_timeout(ANSWER_MAX);
        release.send(()).unwrap();
        assert_eq!(writing.join().unwrap(), Ok(()));
        let (alive, resized, typed) =
            answered.expect("the host answered while one pane's write was stalled");
        assert_eq!(alive, Ok(want));
        assert_eq!(resized, Ok(()));
        assert_eq!(typed, Ok(()));
    });
}
