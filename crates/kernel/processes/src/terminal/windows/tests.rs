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
    others_answer_while_stalled(|host, agent| host.input(agent, STALLED));
}

/// A paste into an app that stops reading its input blocks the same way, and the host still
/// answers every other call while it does, since a paste writes outside the panes lock too.
#[test]
fn a_paste_stalled_on_one_pane_leaves_every_other_call_answering() {
    others_answer_while_stalled(|host, agent| host.paste(agent, STALLED));
}

/// Starts two cmd.exe panes, holds the first pane's writer on the stalled bytes, runs the
/// given write of those bytes to it, and asserts that listing, resizing and typing into the
/// second pane answer within `ANSWER_MAX` before the stall is released. Input: the write.
/// Output: none; panics on a call that did not answer or answered wrong.
fn others_answer_while_stalled(write: fn(&ConptyHost, &AgentId) -> Result<(), HostError>) {
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
        let writing = scope.spawn(move || write(host, stalled));
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

/// How many input calls of `INPUT_BYTES_MAX` bytes the input leg types, well past what a
/// pipe buffer holds.
const INPUT_CALLS: usize = 300;

/// A pane command whose app never reads its input: PING.EXE counting for minutes, named by
/// its absolute path. Input: the folder it starts in. Output: the command.
fn never_reads(cwd: &Path) -> PaneCommand {
    let root = std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("SystemRoot is set on Windows"));
    PaneCommand {
        argv: vec![
            root.join("System32").join("PING.EXE").display().to_string(),
            "-n".into(),
            "600".into(),
            "127.0.0.1".into(),
        ],
        cwd: cwd.to_path_buf(),
        env: Vec::new(),
    }
}

/// Runs one host call on its own thread and hands back its answer, so a call that blocks
/// cannot hold the row past its bound. Inputs: the host, the terminal name and the call.
/// Output: the end that receives the call's answer.
fn answered<T: Send + 'static>(
    host: &Arc<ConptyHost>,
    agent: &AgentId,
    call: fn(&ConptyHost, &AgentId) -> Result<T, HostError>,
) -> Receiver<Result<T, HostError>> {
    let (answer, answers) = channel();
    let (host, agent) = (Arc::clone(host), agent.clone());
    std::thread::spawn(move || {
        let _ = answer.send(call(&host, &agent));
    });
    answers
}

/// A real ConPTY pane whose app never reads its input takes a paste of `PASTE_BYTES_MAX`
/// keys, and another takes `INPUT_CALLS` full input calls, each answering within
/// `ANSWER_MAX`; a kill of either pane then answers within `ANSWER_MAX` too, never waiting
/// on a write.
#[test]
fn a_paste_into_an_app_that_never_reads_returns_and_a_kill_does_not_wait() {
    let temp = tempfile::tempdir().unwrap_or_else(|error| panic!("a temp folder: {error}"));
    let host = Arc::new(ConptyHost::new(|_, _| true));
    let pasted = AgentId::team(1).unwrap_or_else(|error| panic!("team agent 1: {error:?}"));
    let typed = AgentId::team(2).unwrap_or_else(|error| panic!("team agent 2: {error:?}"));
    for agent in [&pasted, &typed] {
        host.spawn(agent, &never_reads(temp.path()))
            .unwrap_or_else(|error| panic!("spawn {agent}: {error:?}"));
    }
    let paste = answered(&host, &pasted, |host, agent| {
        host.paste(agent, &vec![b'a'; PASTE_BYTES_MAX])
    });
    let input = answered(&host, &typed, |host, agent| {
        for _ in 0..INPUT_CALLS {
            host.input(agent, &[b'a'; INPUT_BYTES_MAX])?;
        }
        Ok(())
    });
    let paste_answer = paste.recv_timeout(ANSWER_MAX);
    let input_answer = input.recv_timeout(ANSWER_MAX);
    let killed = [&pasted, &typed]
        .map(|agent| answered(&host, agent, |host, agent| host.kill(agent)))
        .map(|kill| kill.recv_timeout(ANSWER_MAX));
    eprintln!("paste {paste_answer:?} input {input_answer:?} kills {killed:?}");
    assert_eq!(killed, [Ok(Ok(())), Ok(Ok(()))], "a kill waited on a write");
    assert_eq!(
        paste_answer,
        Ok(Ok(())),
        "the paste into an app that never reads"
    );
    assert_eq!(
        input_answer,
        Ok(Ok(())),
        "the input into an app that never reads"
    );
}
