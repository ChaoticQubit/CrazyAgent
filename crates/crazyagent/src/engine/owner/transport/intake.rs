use super::projects_home::{Running, Sink, UNKNOWN};
use super::*;
use crate::projects_home::icloud;
use crate::projects_home::job::{IntakeJob, JobKind, JobState, JobSubject, JobView};
use crate::projects_home::{AddMode, AddOverride, HomeAction};

/// How many times a job reads the current revision and plans its change again when another
/// change moved the revision between the read and the plan.
const REVISION_TRIES: usize = 3;

/// What a client asks a job to do: the change's request id, which names the job, the subject,
/// the mode and the overrides.
pub(super) struct Asked {
    pub request_id: RequestId,
    pub subject: JobSubject,
    pub mode: AddMode,
    pub overrides: Vec<AddOverride>,
}

/// The engine's one job, of any kind: the running job, or the last one ended until it is
/// dismissed or another starts; and how many direct client folder adds and project moves
/// run, read and changed only under the job's lock, so a job and a direct change never run
/// at once.
#[derive(Default)]
pub(super) struct Intake {
    held: Mutex<Option<Held>>,
    direct: std::sync::atomic::AtomicUsize,
}

/// A direct client folder add or project move under way, which keeps any job from starting
/// until it is dropped.
pub(super) struct Direct<'a>(&'a Intake);

impl Drop for Direct<'_> {
    fn drop(&mut self) {
        let _held = self.0.lock();
        let before = self
            .0
            .direct
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        assert!(before > 0, "a direct hold ends once");
    }
}

struct Held {
    job: Arc<IntakeJob>,
    real: PathBuf,
    task: Option<tokio::task::JoinHandle<()>>,
}

impl Held {
    /// Whether this job does what `subject` asks: an add of the same real folder, a move of
    /// the same project, or a delete of the same project. Inputs: the subject and its real
    /// folder. Output: the answer.
    fn same(&self, subject: &JobSubject, real: &Path) -> bool {
        match subject {
            JobSubject::Add { .. } => self.job.kind() == JobKind::Add && self.real == real,
            JobSubject::MoveProject { project_id } => {
                self.job.kind() == JobKind::MoveProject && self.job.project() == Some(project_id)
            }
            JobSubject::Delete { project_id, .. } => {
                self.job.kind() == JobKind::Delete && self.job.project() == Some(project_id)
            }
        }
    }
}

/// What a start finds in the job the engine holds: the same id, or a running job doing the
/// same, answers that job's view; another running job answers `job_busy`; an ended job or
/// none lets the start go on. Inputs: the held job, the asked id, subject and real folder.
/// Output: the answer, or None to go on.
fn found(
    held: Option<&Held>,
    id: &RequestId,
    subject: &JobSubject,
    real: &Path,
) -> Option<Result<JobView, EngineError>> {
    let current = held?;
    let running = !current.job.ended();
    if current.job.id() == id || (running && current.same(subject, real)) {
        return Some(Ok(current.job.view()));
    }
    running.then(|| Err(failure("job_busy")))
}

impl Intake {
    /// Starts a job that runs apart from any client, or attaches to the job already running
    /// for the same subject or with the same id. A project move is checked before it starts,
    /// as far as it can be without reading inside the folder, and a delete as its plan checks
    /// it before its mark (the exact name included), so a change the engine refuses is
    /// answered at once and the held job stays as it was. Inputs: the engine, the
    /// authority, the client packet, its admission and what to do. Output: the job's view,
    /// `job_busy` while another job, a direct client add or project move, or the id as a
    /// client change runs, or the folder, override or move check's code.
    pub(super) async fn start(
        engine: &Arc<Engine>,
        authority: &Arc<Authority>,
        packet: &[u8; 37],
        a: &Admission,
        asked: Asked,
    ) -> Result<JobView, EngineError> {
        let (project, source, real) = match &asked.subject {
            JobSubject::Add { path } => {
                HomeAction::Added {
                    source: path.clone(),
                    mode: asked.mode,
                    overrides: asked.overrides.clone(),
                }
                .validate()?;
                (None, path.clone(), icloud::folder(path)?)
            }
            JobSubject::MoveProject { project_id } => {
                HomeAction::MoveProject {
                    project_id: project_id.clone(),
                    mode: asked.mode,
                    overrides: asked.overrides.clone(),
                }
                .validate()?;
                let early = {
                    let held = authority.intake.lock();
                    found(
                        held.as_ref(),
                        &asked.request_id,
                        &asked.subject,
                        Path::new(""),
                    )
                };
                if let Some(answer) = early {
                    return answer;
                }
                let source = check_move(engine, a, project_id, asked.mode).await?;
                let real = PathBuf::from(&source);
                (Some(project_id.clone()), source, real)
            }
            JobSubject::Delete { project_id, name } => {
                if name.is_empty() || !asked.overrides.is_empty() {
                    return Err(failure("job_request_invalid"));
                }
                let early = {
                    let held = authority.intake.lock();
                    found(
                        held.as_ref(),
                        &asked.request_id,
                        &asked.subject,
                        Path::new(""),
                    )
                };
                if let Some(answer) = early {
                    return answer;
                }
                let folder = check_delete(engine, a, project_id, name).await?;
                (
                    Some(project_id.clone()),
                    name.clone(),
                    PathBuf::from(folder),
                )
            }
        };
        assert!(real.is_absolute());
        let mut held = authority.intake.lock();
        if let Some(answer) = found(held.as_ref(), &asked.request_id, &asked.subject, &real) {
            return answer;
        }
        if authority.intake.directs() > 0 {
            return Err(failure("job_busy"));
        }
        let Some(claim) = Running::claim(authority, &a.owner_id, &asked.request_id) else {
            return Err(failure("job_busy"));
        };
        let replaced = held.as_ref().map(|ended| ended.job.id().clone());
        let job = Arc::new(IntakeJob::new(
            asked.request_id,
            asked.subject.kind(),
            project,
            source,
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
    /// was running; its view then reads cancelled, or done when its folder had landed; or
    /// `delete_cannot_stop` for a delete that committed to its first Trash move.
    pub(super) fn stop(&self, id: &RequestId) -> Result<bool, EngineError> {
        let held = self.lock();
        match held.as_ref() {
            Some(current) if current.job.id() == id && !current.job.ended() => {
                if current.job.stop() {
                    return Ok(true);
                }
                assert_eq!(current.job.kind(), JobKind::Delete);
                Err(failure("delete_cannot_stop"))
            }
            _ => Ok(false),
        }
    }

    /// Reads the job an engine stop cut short, a project move first, else the newest add, so
    /// status shows how it ended until the owner dismisses it or a new job starts. Runs at
    /// server start, after the storage start finished any cut move from disk; a job a client
    /// started first wins. Inputs: the engine and the wait. Output: none; a failed read is
    /// logged and shows nothing.
    pub(super) async fn restore(&self, engine: &Engine, wait: Duration) {
        let Some((job, real)) = restored(engine, wait).await else {
            return;
        };
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

    /// Forgets an ended job, so status reads none. Input: its id. Output: the kind of the
    /// ended job forgotten, or None; a running job is never forgotten.
    pub(super) fn dismiss(&self, id: &RequestId) -> Option<JobKind> {
        let mut held = self.lock();
        let ended = held
            .as_ref()
            .filter(|current| current.job.id() == id && current.job.ended())
            .map(|current| current.job.kind());
        if ended.is_some() {
            *held = None;
        }
        assert!(ended.is_none() || held.is_none());
        ended
    }

    /// Forgets the delete cut short the slot shows for a project once the owner kept the
    /// project from its banner, so Home stops offering it. Input: the project id. Output:
    /// whether it was forgotten.
    pub(super) fn forget_cut_short(&self, project: &str) -> bool {
        let mut held = self.lock();
        let cut = held.as_ref().is_some_and(|current| {
            current.job.kind() == JobKind::Delete
                && current
                    .job
                    .project()
                    .is_some_and(|id| id.as_str() == project)
                && matches!(current.job.view().state, JobState::CutShort { .. })
        });
        if cut {
            *held = None;
        }
        assert!(!cut || held.is_none());
        cut
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

    /// Holds the slot for a direct client folder add or project move while it runs, so it
    /// never runs beside a job and no job starts until it ends. Output: the hold, dropped
    /// when the change ends, or `job_busy` while a job runs.
    pub(super) fn direct(&self) -> Result<Direct<'_>, EngineError> {
        let held = self.lock();
        if held.as_ref().is_some_and(|current| !current.job.ended()) {
            return Err(failure("job_busy"));
        }
        let before = self
            .direct
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        assert!(before < usize::MAX);
        drop(held);
        Ok(Direct(self))
    }

    /// How many direct client folder adds and project moves run; read under the job's lock
    /// by a start. Output: the count.
    fn directs(&self) -> usize {
        self.direct.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Held>> {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The job an engine stop cut short, as the storage start kept it: a project move, else a
/// Delete everything (whose real folder is never compared, so it is left empty), else the
/// newest add. Inputs: the engine and the wait for each read. Output: the ended job and its
/// real folder, or None when there is none or a read failed, which is logged.
async fn restored(engine: &Engine, wait: Duration) -> Option<(IntakeJob, PathBuf)> {
    let moved = engine.owner_job(Job::HomeProjectMoveEnded);
    match tokio::time::timeout(wait, moved).await {
        Ok(Ok(Some(ended))) => {
            let real = PathBuf::from(&ended.source);
            let job = IntakeJob::restored(
                ended.request,
                JobKind::MoveProject,
                Some(ended.project),
                ended.source,
                ended.mode,
                &ended.ended,
            );
            return Some((job, real));
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => eprintln!("move_project_not_restored cause={}", e.0),
        Err(_) => eprintln!("move_project_not_restored cause=timeout"),
    }
    let cut = engine.owner_job(Job::HomeDeleteCutShort);
    match tokio::time::timeout(wait, cut).await {
        Ok(Ok(Some((found, trashed)))) => {
            let job = IntakeJob::cut_short(
                found.request,
                found.project,
                found.name,
                trashed,
                found.total,
            );
            return Some((job, PathBuf::new()));
        }
        Ok(Ok(None)) => {}
        Ok(Err(e)) => eprintln!("project_delete_not_restored cause={}", e.0),
        Err(_) => eprintln!("project_delete_not_restored cause=timeout"),
    }
    let read = engine.owner_job(Job::HomeAddEnded);
    let ended = match tokio::time::timeout(wait, read).await {
        Ok(Ok(Some(ended))) => ended,
        Ok(Ok(None)) => return None,
        Ok(Err(e)) => {
            eprintln!("home_add_not_restored cause={}", e.0);
            return None;
        }
        Err(_) => {
            eprintln!("home_add_not_restored cause=timeout");
            return None;
        }
    };
    let real = PathBuf::from(&ended.source);
    let job = IntakeJob::restored(
        ended.request,
        JobKind::Add,
        None,
        ended.source,
        ended.mode,
        &ended.ended,
    );
    Some((job, real))
}

/// Runs one job: forgets the ended job it replaced, does the job's work, then records how
/// the job ended. The claim on the job's id is dropped before the end is recorded, so a new
/// job that may start then never finds two job claims held. Inputs: the engine, the
/// authority, the packet and admission of the client that started it, the job, its claim
/// and the ended job it replaced.
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
            .owner_job(|reply| Job::HomeJobForget(old, reply))
            .await
    {
        eprintln!("job_not_forgotten cause={}", e.0);
    }
    let result = worked(&engine, &authority, &packet, &a, &job).await;
    drop(claim);
    ending.end(result);
    assert!(job.ended());
}

/// The job's work in order. An add: the check, the download, then the add planned in full
/// again right before its copy. A project move, checked at its start: the download of what
/// of the project's folder is only in iCloud, then the move planned in full, which waits
/// for the project's agents before anything moves. A delete, checked at its start: the
/// delete planned in full, which waits for the agents, then moves the folders to the Trash.
/// Inputs: as [`run`]. Output: Ok once the change is recorded as applied, else the first
/// step's code.
async fn worked(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    job: &Arc<IntakeJob>,
) -> Result<(), EngineError> {
    if job.kind() == JobKind::Delete {
        job.copying();
        return served(engine, authority, packet, a, job).await;
    }
    if job.kind() == JobKind::Add {
        check(engine, a, job).await?;
    }
    fetch(authority, job).await?;
    job.copying();
    served(engine, authority, packet, a, job).await
}

/// Checks the job's add before its download, so a folder the add refuses is not downloaded
/// first: every refusal that reads nothing inside the folder, with no add row written. A
/// soft refusal the folder's files decide still comes after the download. Inputs: the
/// engine, the job's admission and the job. Output: Ok, the refusal code, or the
/// unknown-outcome code when storage does not answer in time.
async fn check(engine: &Engine, a: &Admission, job: &IntakeJob) -> Result<(), EngineError> {
    assert_eq!(job.kind(), JobKind::Add);
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

/// Checks a Delete everything before its job starts, writing nothing. Inputs: the engine, the
/// client's admission, the project and the name the owner typed. Output: the project's
/// folder, the refusal code, or the unknown-outcome code when storage does not answer in time.
async fn check_delete(
    engine: &Engine,
    a: &Admission,
    project: &ProjectId,
    name: &str,
) -> Result<String, EngineError> {
    let ask = engine.owner_job(|reply| {
        Job::HomeDeleteCheck(a.clone(), project.clone(), name.to_owned(), reply)
    });
    let checked = tokio::time::timeout(engine.timeout, ask)
        .await
        .map_err(|_| failure(UNKNOWN))?;
    assert!(!matches!(&checked, Err(e) if e.0.is_empty()));
    assert!(!matches!(&checked, Ok(folder) if folder.is_empty()));
    checked
}

/// Checks a project move before its job starts, writing nothing. Inputs: the engine, the
/// client's admission, the project and the mode. Output: the project's real folder, the
/// refusal code, or the unknown-outcome code when storage does not answer in time.
async fn check_move(
    engine: &Engine,
    a: &Admission,
    project: &ProjectId,
    mode: AddMode,
) -> Result<String, EngineError> {
    let ask = engine
        .owner_job(|reply| Job::HomeProjectMoveCheck(a.clone(), project.clone(), mode, reply));
    let checked = tokio::time::timeout(engine.timeout, ask)
        .await
        .map_err(|_| failure(UNKNOWN))?;
    assert!(!matches!(&checked, Err(e) if e.0.is_empty()));
    assert!(!matches!(&checked, Ok(source) if source.is_empty()));
    checked
}

/// A job's end, recorded once: by its task, or when the task stops early, an abort at
/// engine stop included, as stopped and its kind's interrupted code.
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
            self.0.end(Err(failure(self.0.interrupted())));
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

/// Serves the job's change, planning it again while another change moved the revision
/// between the read and the plan. Inputs: the engine, the authority, the packet, the
/// admission and the job. Output: Ok once the change is recorded as applied, else its code.
async fn served(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    job: &IntakeJob,
) -> Result<(), EngineError> {
    let mut result = Err(failure("stale_revision"));
    for _ in 0..REVISION_TRIES {
        result = served_once(engine, authority, packet, a, job).await;
        if !matches!(&result, Err(e) if e.0 == "stale_revision") {
            break;
        }
    }
    result
}

/// Reads the current revision and serves the job's change once, with a fresh owner deadline
/// taken after the download: an AddExistingFolder for an add, a MoveProjectHome for a
/// project move, a DeleteProject with the typed name for a delete. Inputs: as [`served`].
/// Output: as [`served`], `stale_revision` included.
async fn served_once(
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
    let mutation = match (job.kind(), job.project()) {
        (JobKind::Delete, Some(project)) => OwnerMutation::DeleteProject {
            project_id: project.clone(),
            name: job.source().to_owned(),
        },
        (_, None) => OwnerMutation::AddExistingFolder {
            source: job.source().to_owned(),
            mode: job.mode(),
            overrides: job.overrides().to_vec(),
        },
        (_, Some(project)) => OwnerMutation::MoveProjectHome {
            project_id: project.clone(),
            mode: job.mode(),
            overrides: job.overrides().to_vec(),
        },
    };
    assert_eq!(
        matches!(mutation, OwnerMutation::MoveProjectHome { .. }),
        job.kind() == JobKind::MoveProject
    );
    assert_eq!(
        matches!(mutation, OwnerMutation::DeleteProject { .. }),
        job.kind() == JobKind::Delete
    );
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: job.id().clone(),
        expected_revision: current.revision,
        operation: OwnerOperation::Apply { mutation },
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

/// The job's result from its change's reply. Input: the kept reply. Output: Ok for a change
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
