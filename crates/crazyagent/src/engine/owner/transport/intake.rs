use super::projects_home::{Running, Sink, UNKNOWN};
use super::*;
use crate::projects_home::icloud;
use crate::projects_home::job::{IntakeJob, JobView};
use crate::projects_home::{AddMode, AddOverride, HomeAction};

/// How many times a job reads the current revision and plans its add again when another
/// change moved the revision between the read and the plan.
const REVISION_TRIES: usize = 3;

/// What a client asks an intake job to add: the add's request id, which names the job, the
/// folder text, the mode and the overrides.
pub(super) struct Asked {
    pub request_id: RequestId,
    pub path: String,
    pub mode: AddMode,
    pub overrides: Vec<AddOverride>,
}

/// The engine's one intake job: the running job, or the last one ended until it is
/// dismissed or another starts.
#[derive(Default)]
pub(super) struct Intake(Mutex<Option<Held>>);

struct Held {
    job: Arc<IntakeJob>,
    real: PathBuf,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Intake {
    /// Starts an intake job that runs apart from any client, or attaches to the job already
    /// running for the same real folder or with the same id. Inputs: the engine, the
    /// authority, the client packet, its admission and what to add. Output: the job's view,
    /// `intake_busy` while a job for another folder runs or the id runs as a client change,
    /// or the folder or override check's code.
    pub(super) fn start(
        engine: &Arc<Engine>,
        authority: &Arc<Authority>,
        packet: &[u8; 37],
        a: &Admission,
        asked: Asked,
    ) -> Result<JobView, EngineError> {
        HomeAction::Added {
            source: asked.path.clone(),
            mode: asked.mode,
            overrides: asked.overrides.clone(),
        }
        .validate()?;
        let real = icloud::folder(&asked.path)?;
        let mut held = authority.intake.lock();
        if let Some(current) = held.as_ref() {
            let running = !current.job.ended();
            if current.job.id() == &asked.request_id || (running && current.real == real) {
                return Ok(current.job.view());
            }
            if running {
                return Err(failure("intake_busy"));
            }
        }
        let Some(claim) = Running::claim(authority, &a.owner_id, &asked.request_id) else {
            return Err(failure("intake_busy"));
        };
        let replaced = held.as_ref().map(|ended| ended.job.id().clone());
        let job = Arc::new(IntakeJob::new(
            asked.request_id,
            a.owner_id.clone(),
            asked.path,
            asked.mode,
            asked.overrides,
        ));
        let task = tokio::spawn(run(
            Arc::clone(engine),
            Arc::clone(authority),
            *packet,
            a.clone(),
            Arc::clone(&job),
            claim,
            replaced,
        ));
        let view = job.view();
        let task = Some(task);
        *held = Some(Held { job, real, task });
        assert!(held.is_some());
        Ok(view)
    }

    /// The job, running or last ended. Output: its view, or None when there is none.
    pub(super) fn status(&self) -> Option<JobView> {
        self.lock().as_ref().map(|held| held.job.view())
    }

    /// Asks the running job to stop between files. Input: its id. Output: whether that job
    /// was running; its view then reads cancelled, or done when its folder had landed.
    pub(super) fn stop(&self, id: &RequestId) -> bool {
        let held = self.lock();
        match held.as_ref() {
            Some(current) if current.job.id() == id && !current.job.ended() => {
                current.job.stop();
                true
            }
            _ => false,
        }
    }

    /// Reads the newest add an engine stop cut short, so status shows how it ended until
    /// the owner dismisses it or a new job starts. Runs at server start; a job a client
    /// started first wins. Inputs: the engine and the wait. Output: none; a failed read is
    /// logged and shows nothing.
    pub(super) async fn restore(&self, engine: &Engine, wait: Duration) {
        let read = engine.owner_job(Job::HomeAddEnded);
        let ended = match tokio::time::timeout(wait, read).await {
            Ok(Ok(Some(ended))) => ended,
            Ok(Ok(None)) => return,
            Ok(Err(e)) => {
                eprintln!("home_add_not_restored cause={}", e.0);
                return;
            }
            Err(_) => {
                eprintln!("home_add_not_restored cause=timeout");
                return;
            }
        };
        let real = PathBuf::from(&ended.source);
        let job = IntakeJob::restored(
            ended.request,
            ended.owner,
            ended.source,
            ended.mode,
            &ended.ended,
        );
        assert!(job.ended());
        let mut held = self.lock();
        if held.is_none() {
            let job = Arc::new(job);
            *held = Some(Held {
                job,
                real,
                task: None,
            });
        }
    }

    /// Forgets an ended job, so status reads none. Input: its id. Output: whether that
    /// ended job was forgotten; a running job is never forgotten.
    pub(super) fn dismiss(&self, id: &RequestId) -> bool {
        let mut held = self.lock();
        let ended = held
            .as_ref()
            .is_some_and(|current| current.job.id() == id && current.job.ended());
        if ended {
            *held = None;
        }
        assert!(!ended || held.is_none());
        ended
    }

    /// Stops the running job as the engine stops, after its clients ended, so none starts
    /// another: asks it to stop, waits up to `wait` for its copy to end between files and
    /// its end to be recorded, then aborts its task. Input: the wait.
    pub(super) async fn stop_all(&self, wait: Duration) {
        let Some(Held { job, task, .. }) = self.lock().take() else {
            return;
        };
        assert!(self.lock().is_none());
        let Some(task) = task else {
            return;
        };
        job.stop();
        let abort = task.abort_handle();
        if tokio::time::timeout(wait, task).await.is_err() {
            abort.abort();
            eprintln!("intake_job_abandoned_at_stop");
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Held>> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Runs one intake job: forgets the ended add it replaced, checks the add before any
/// download, downloads what of the folder is only in iCloud, then serves the add with the
/// job as its sink and records how the job ended. The claim on the job's id is dropped
/// before the end is recorded, so a new job that may start then never finds two job claims
/// held. Inputs: the engine, the authority, the packet and admission of the client that
/// started it, the job, its claim and the ended job it replaced.
async fn run(
    engine: Arc<Engine>,
    authority: Arc<Authority>,
    packet: [u8; 37],
    a: Admission,
    job: Arc<IntakeJob>,
    claim: Running,
    replaced: Option<RequestId>,
) {
    let ending = Ending(Arc::clone(&job));
    if let Some(old) = replaced
        && let Err(e) = engine
            .owner_job(|reply| Job::HomeAddForget(old, reply))
            .await
    {
        eprintln!("home_add_not_forgotten cause={}", e.0);
    }
    let result = added(&engine, &authority, &packet, &a, &job).await;
    drop(claim);
    ending.end(result);
    assert!(job.ended());
}

/// The job's work in order: the check, the download, then the add planned in full again
/// right before its copy. Inputs: as [`run`]. Output: Ok once the add is recorded as
/// applied, else the first step's code.
async fn added(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    job: &Arc<IntakeJob>,
) -> Result<(), EngineError> {
    check(engine, a, job).await?;
    fetch(authority, job).await?;
    job.copying();
    add(engine, authority, packet, a, job).await
}

/// Checks the job's add before its download, so a folder the add refuses is not downloaded
/// first: every refusal that reads nothing inside the folder, with no add row written. A
/// soft refusal the folder's files decide still comes after the download. Inputs: the
/// engine, the job's admission and the job. Output: Ok, the refusal code, or the
/// unknown-outcome code when storage does not answer in time.
async fn check(engine: &Engine, a: &Admission, job: &IntakeJob) -> Result<(), EngineError> {
    let (id, source) = (job.id().clone(), job.source().to_owned());
    let (mode, overrides) = (job.mode(), job.overrides().to_vec());
    assert!(!source.is_empty());
    let ask =
        engine.owner_job(|reply| Job::HomeAddCheck(a.clone(), id, source, mode, overrides, reply));
    let checked = tokio::time::timeout(engine.timeout, ask)
        .await
        .map_err(|_| failure(UNKNOWN))?;
    assert!(!matches!(&checked, Err(e) if e.0.is_empty()));
    checked
}

/// A job's end, recorded once: by its task, or when the task stops early, an abort at
/// engine stop included, as stopped and `home_add_interrupted`.
struct Ending(Arc<IntakeJob>);

impl Ending {
    fn end(self, result: Result<(), EngineError>) {
        assert!(!self.0.ended());
        self.0.end(result);
    }
}

impl Drop for Ending {
    fn drop(&mut self) {
        if !self.0.ended() {
            self.0.stop();
            self.0.end(Err(failure("home_add_interrupted")));
        }
        assert!(self.0.ended());
    }
}

/// Downloads what of the job's folder is only in iCloud, holding the engine's download slot.
/// Input: the authority and the job. Output: Ok once every file is on this Mac, else the
/// download's code, `icloud_download_busy` while another download runs.
async fn fetch(authority: &Arc<Authority>, job: &Arc<IntakeJob>) -> Result<(), EngineError> {
    let claim = authority.icloud.claim_for(job.download())?;
    let download = claim.download();
    let path = job.source().to_owned();
    let bounds = crate::projects_home::add_bounds(job.overrides());
    let fetched = tokio::task::spawn_blocking(move || icloud::download(&path, &download, bounds))
        .await
        .map_err(|_| failure("home_icloud_not_downloaded"));
    drop(claim);
    let files = fetched??;
    assert!(files <= bounds.entries);
    Ok(())
}

/// Serves the job's add, planning it again while another change moved the revision between
/// the read and the plan. Inputs: the engine, the authority, the packet, the admission and
/// the job. Output: Ok once the add is recorded as applied, else its code.
async fn add(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    job: &IntakeJob,
) -> Result<(), EngineError> {
    let mut result = Err(failure("stale_revision"));
    for _ in 0..REVISION_TRIES {
        result = add_once(engine, authority, packet, a, job).await;
        if !matches!(&result, Err(e) if e.0 == "stale_revision") {
            break;
        }
    }
    result
}

/// Reads the current revision and serves the add once, with a fresh owner deadline taken
/// after the download. Inputs: as [`add`]. Output: as [`add`], `stale_revision` included.
async fn add_once(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    job: &IntakeJob,
) -> Result<(), EngineError> {
    let deadline = Instant::now() + engine.timeout;
    let read = engine.owner_job(|reply| Job::OwnerRead(a.clone(), OwnerRead::ProjectsHome, reply));
    let current = tokio::time::timeout_at(deadline, read)
        .await
        .map_err(|_| failure(UNKNOWN))??;
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: job.id().clone(),
        expected_revision: current.revision,
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::AddExistingFolder {
                source: job.source().to_owned(),
                mode: job.mode(),
                overrides: job.overrides().to_vec(),
            },
        },
    };
    request.validate()?;
    assert!(projects_home::handles(&request));
    let mut kept = None;
    let sink = Sink::Job(job.tally(), &mut kept);
    let served = projects_home::serve(sink, engine, authority, packet, a, request, deadline).await;
    match kept {
        Some(reply) => applied(reply),
        None => Err(served.err().unwrap_or_else(|| failure(UNKNOWN))),
    }
}

/// The job's result from its add's reply. Input: the kept reply. Output: Ok for an add
/// recorded as applied, else its refusal code, or the unknown-outcome code.
fn applied(reply: Result<OwnerReply, EngineError>) -> Result<(), EngineError> {
    let result = match reply?.response {
        OwnerResponse::Applied { operation } => match operation.result {
            OwnerResult::Applied => Ok(()),
            OwnerResult::Rejected { code } => Err(EngineError(code)),
            _ => Err(failure(UNKNOWN)),
        },
        OwnerResponse::Rejected { code } => Err(EngineError(code)),
        _ => Err(failure(UNKNOWN)),
    };
    assert!(!matches!(&result, Err(e) if e.0.is_empty()));
    result
}
