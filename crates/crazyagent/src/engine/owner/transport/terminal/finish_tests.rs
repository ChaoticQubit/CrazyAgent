use super::*;
use kernel_processes::terminal::Host;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(10);

/// How long the stalled paste holds at most before it answers anyway, so a failed row never
/// leaves a blocking thread behind. Longer than a view may take to close, so the row reads
/// the close, not the hold's end.
const HELD: Duration = Duration::from_secs(40);

/// A host whose paste reports that it began and then blocks until the row lets it go, at
/// most `HELD`, as a paste into a pane whose app stopped reading its input blocks; every
/// other call answers at once.
struct Stalled {
    began: tokio::sync::mpsc::UnboundedSender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl Host for Stalled {
    fn spawn(
        &self,
        _: &AgentId,
        _: &kernel_processes::terminal::PaneCommand,
    ) -> Result<kernel_processes::terminal::PaneRef, HostError> {
        Err(HostError("terminal_unavailable".into()))
    }

    fn attach(&self, _: &AgentId) -> Result<TerminalStream, HostError> {
        Err(HostError("terminal_agent_unknown".into()))
    }

    fn input(&self, _: &AgentId, _: &[u8]) -> Result<(), HostError> {
        Ok(())
    }

    fn paste(&self, _: &AgentId, _: &[u8]) -> Result<(), HostError> {
        let _ = self.began.send(());
        let _ = self.release.lock().unwrap().recv_timeout(HELD);
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

/// A grant that stands until the row ends it, then no longer allows or holds, as an owner
/// capability changed on disk reads at the view's next grant check.
#[derive(Default)]
struct Revocable {
    ended: AtomicBool,
}

impl Viewing for Revocable {
    fn allows(&self) -> bool {
        !self.ended.load(Ordering::SeqCst)
    }

    async fn holds(&self) -> bool {
        !self.ended.load(Ordering::SeqCst)
    }

    fn typed(&self, _: &TerminalRequest) {}

    async fn heard(&self, _: String) {}
}

/// A view whose grant ends while its paste is stalled in the host closes as
/// `owner_grant_stale_or_revoked` within one grant check and `WRITE_TIME`, instead of
/// waiting on the stalled write for as long as it lasts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_paste_stalled_when_the_grant_ends_still_ends_the_view() {
    let close_max = GRANT_CHECK + WRITE_TIME + Duration::from_secs(5);
    assert!(HELD > close_max + WAIT, "the hold outlasts the close it is measured against");
    let (began, mut begun) = tokio::sync::mpsc::unbounded_channel();
    let (release, released) = std::sync::mpsc::channel();
    let host: Arc<dyn Host> = Arc::new(Stalled {
        began,
        release: std::sync::Mutex::new(released),
    });
    let agent = AgentId::team(1).unwrap();
    let (_feed, chunks) = kernel_processes::terminal::buffer::buffer();
    let viewer = TerminalStream {
        agent: agent.clone(),
        cols: 80,
        rows: 24,
        history: Vec::new(),
        screen: Vec::new(),
        chunks,
        at: None,
    };
    let seated =
        seat(&host, &Arc::default(), &agent, None, false).expect("a stub host names a unit");
    let viewing = Arc::new(Revocable::default());
    let (requests, mut asked) = tokio::sync::mpsc::channel(REQUESTS_QUEUED);
    let (mut ours, _theirs) = tokio::io::duplex(1 << 16);
    let pumping = {
        let viewing = viewing.clone();
        tokio::spawn(async move {
            pump(&mut ours, View::of(viewer, seated), &mut asked, host, &*viewing).await
        })
    };
    requests
        .send(TerminalRequest::Paste {
            bytes: b"stalled paste".to_vec(),
            last: true,
        })
        .await
        .unwrap();
    assert_eq!(
        tokio::time::timeout(WAIT, begun.recv()).await,
        Ok(Some(())),
        "the paste reached the host"
    );

    viewing.ended.store(true, Ordering::SeqCst);
    let started = Instant::now();
    let ended = tokio::time::timeout(close_max, pumping).await;
    let _ = release.send(());
    println!("finish row: the view ended after {:?}", started.elapsed());
    assert_eq!(
        ended.map(Result::ok),
        Ok(Some(GRANT_ENDED)),
        "the view ended at its grant check, not after the stalled paste"
    );
    drop(requests);
}
