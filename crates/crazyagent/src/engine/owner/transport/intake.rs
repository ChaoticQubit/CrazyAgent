use super::projects_home::{Running, Sink, UNKNOWN};
use super::*;
use crate::projects_home::icloud;
use crate::projects_home::job::{IntakeJob, JobKind, JobState, JobSubject, JobView};
use crate::projects_home::{AddMode, AddOverride, HomeAction};

/// How many times a job reads the current revision and plans its change again when another
/// change moved the revision between the read and the plan.
const REVISION_TRIES: usize = 3;

/// The most jobs that run at once (DEC-197). Each other running job's record may move the
/// revision once between a job's read and its plan, so the bound stays within the tries.
pub(super) const JOBS_RUNNING_MAX: usize = 3;

/// The most jobs that wait for their turn beyond the running ones (DEC-197); a start past
/// them is refused `job_busy`.
pub(super) const JOBS_WAITING_MAX: usize = 8;

const _: () = assert!(JOBS_RUNNING_MAX <= REVISION_TRIES);

/// What a client asks a job to do: the change's request id, which names the job, the subject,
/// the mode and the overrides.
pub(super) struct Asked {
    pub request_id: RequestId,
    pub subject: JobSubject,
    pub mode: AddMode,
    pub overrides: Vec<AddOverride>,
}

/// The engine's jobs, of any kind, in start order: the running ones, the ones waiting for
/// their turn, and the ones ended since the last start until they are dismissed or another
/// starts; and how many direct client folder adds and project moves run, read and changed
/// only under the jobs' lock, so a job and a direct change never run at once. Adds onto
/// names apart run side by side; any other job runs alone (DEC-197).
#[derive(Default)]
pub(super) struct Intake {
    held: Mutex<Vec<Held>>,
    direct: std::sync::atomic::AtomicUsize,
    downloads: tokio::sync::Mutex<()>,
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
    turn: Option<oneshot::Sender<()>>,
}

impl Held {
    /// Whether the job has not ended: running or waiting for its turn.
    fn live(&self) -> bool {
        !self.job.ended()
    }

    /// Whether the job waits for its turn: its task has not been let go.
    fn waiting(&self) -> bool {
        self.turn.is_some()
    }

    /// Whether the job runs: let go and not ended.
    fn running(&self) -> bool {
        self.live() && !self.waiting()
    }

    /// Whether this job may run beside `other`: two adds whose folders land under names the
    /// volume tells apart. Input: the other job. Output: the answer.
    fn beside(&self, other: &Held) -> bool {
        let name = |held: &Held| held.real.file_name().and_then(std::ffi::OsStr::to_str);
        self.job.kind() == JobKind::Add
            && other.job.kind() == JobKind::Add
            && matches!((name(self), name(other)), (Some(a), Some(b)) if crate::projects_home::names_apart(a, b))
    }

    /// Whether this job's folder and `real` are the same, or one holds the other. An empty
    /// folder, as a start's early check gives, overlaps nothing. Input: the folder. Output: the
    /// answer.
    fn overlaps(&self, real: &Path) -> bool {
        let known = self.real.is_absolute() && real.is_absolute();
        known && (self.real.starts_with(real) || real.starts_with(&self.real))
    }

    /// Whether this job does what `subject` asks: an add of the same real folder, a move of
    /// the same project, a delete of the same project, or a New Project of the same project.
    /// Inputs: the subject and its real folder. Output: the answer.
    fn same(&self, subject: &JobSubject, real: &Path) -> bool {
        match subject {
            JobSubject::Add { .. } => self.job.kind() == JobKind::Add && self.real == real,
            JobSubject::MoveProject { project_id } => {
                self.job.kind() == JobKind::MoveProject && self.job.project() == Some(project_id)
            }
            JobSubject::Delete { project_id, .. } => {
                self.job.kind() == JobKind::Delete && self.job.project() == Some(project_id)
            }
            JobSubject::Setup { project_id, .. } => {
                self.job.kind() == JobKind::Setup && self.job.project() == Some(project_id)
            }
        }
    }
}

/// What a start finds in the jobs the engine holds: a job with the same id answers its view;
/// a live job moving, deleting or building the same project answers its view; a live job on
/// the same folder as an add, or on a folder inside or around the asked one, answers
/// `job_same_folder`; anything else lets the start go on. Inputs: the held jobs, the asked
/// id, subject and real folder. Output: the answer, or None to go on.
fn found(
    held: &[Held],
    id: &RequestId,
    subject: &JobSubject,
    real: &Path,
) -> Option<Result<JobView, EngineError>> {
    if let Some(current) = held.iter().find(|current| current.job.id() == id) {
        return Some(Ok(current.job.view()));
    }
    let live = held.iter().filter(|current| current.live());
    for current in live {
        let adding = matches!(subject, JobSubject::Add { .. });
        if current.same(subject, real) && !adding {
            return Some(Ok(current.job.view()));
        }
        if current.same(subject, real) || current.overlaps(real) {
            return Some(Err(failure("job_same_folder")));
        }
    }
    None
}

/// Lets go every waiting job whose turn came, in start order: one runs when fewer than
/// `JOBS_RUNNING_MAX` run, it may run beside each of them, and no earlier waiting job it may
/// not run beside is still waiting, so a job running alone is never passed for good. Every
/// job left waiting names the job it waits behind. Input: the held jobs.
fn admit(held: &mut [Held]) {
    for at in 0..held.len() {
        if !held[at].waiting() {
            continue;
        }
        let (before, rest) = held.split_at_mut(at);
        let (current, after) = rest.split_first_mut().expect("the job at `at` is held");
        let others = || before.iter().chain(after.iter());
        let running = others().filter(|other| other.running()).count();
        let blocker = others()
            .filter(|other| other.running())
            .find(|other| !current.beside(other))
            .or_else(|| {
                before
                    .iter()
                    .find(|other| other.waiting() && !current.beside(other))
            });
        match blocker {
            None if running < JOBS_RUNNING_MAX => {
                current.job.go();
                let turn = current.turn.take().expect("a waiting job holds its turn");
                let _ = turn.send(());
            }
            None => {
                let first = others().find(|other| other.running());
                let first = first.expect("a full set of running jobs has a first");
                current.job.wait(first.job.label());
            }
            Some(blocker) => current.job.wait(blocker.job.label()),
        }
    }
    let running = held.iter().filter(|held| held.running()).count();
    assert!(running <= JOBS_RUNNING_MAX);
}

impl Intake {
    /// Starts a job that runs apart from any client, or attaches to the job already running
    /// for the same subject or with the same id. A project move is checked before it starts,
    /// as far as it can be without reading inside the folder, a delete as its plan checks
    /// it before its mark (the exact name included) and a New Project as its plan checks it
    /// before it builds anything, so a change the engine refuses is answered at once and the
    /// held job stays as it was. Inputs: the engine, the
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
                    found(&held, &asked.request_id, &asked.subject, Path::new(""))
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
                    found(&held, &asked.request_id, &asked.subject, Path::new(""))
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
            JobSubject::Setup {
                project_id,
                spec,
                setup,
            } => {
                if !asked.overrides.is_empty() {
                    return Err(failure("job_request_invalid"));
                }
                let early = {
                    let held = authority.intake.lock();
                    found(&held, &asked.request_id, &asked.subject, Path::new(""))
                };
                if let Some(answer) = early {
                    return answer;
                }
                let action = crate::project_setup::SetupAction::Create {
                    project_id: project_id.clone(),
                    spec: spec.clone(),
                    setup: Box::new(setup.clone()),
                };
                let folder = check_setup(engine, a, action).await?;
                (
                    Some(project_id.clone()),
                    spec.name.clone(),
                    PathBuf::from(folder),
                )
            }
        };
        assert!(real.is_absolute());
        let mut held = authority.intake.lock();
        if let Some(answer) = found(&held, &asked.request_id, &asked.subject, &real) {
            return answer;
        }
        if authority.intake.directs() > 0 {
            return Err(failure("job_busy"));
        }
        let waiting = held.iter().filter(|current| current.waiting()).count();
        assert!(waiting <= JOBS_WAITING_MAX);
        let taken = Running::claim(authority, &a.owner_id, &asked.request_id);
        let Some(claim) = trace::claim(&asked.request_id, taken) else {
            return Err(failure("job_busy"));
        };
        let job = Arc::new(IntakeJob::new(
            asked.request_id.clone(),
            asked.subject.kind(),
            project,
            source,
            asked.mode,
            asked.overrides,
        ));
        let (turn, turned) = oneshot::channel();
        held.push(Held {
            job: Arc::clone(&job),
            real,
            task: None,
            turn: Some(turn),
        });
        admit(&mut held);
        let queued = held.last().expect("the start was pushed").waiting();
        let waiting = held.iter().filter(|current| current.waiting()).count();
        if queued && waiting > JOBS_WAITING_MAX {
            held.pop();
            return Err(failure("job_busy"));
        }
        let mut replaced = Vec::new();
        held.retain(|current| {
            let kept = current.live();
            if !kept {
                replaced.push(current.job.id().clone());
            }
            kept
        });
        let live = held.iter().filter(|current| current.live()).count();
        assert_eq!(held.len(), live, "a start keeps only live jobs");
        assert!(live <= JOBS_RUNNING_MAX + JOBS_WAITING_MAX);
        let task = tokio::spawn(run(
            Arc::clone(engine),
            Arc::clone(authority),
            *packet,
            a.clone(),
            Arc::clone(&job),
            asked.subject,
            claim,
            replaced,
            turned,
        ));
        let view = job.view();
        let current = held.last_mut().expect("the start is held last");
        assert_eq!(current.job.id(), &asked.request_id);
        current.task = Some(task);
        Ok(view)
    }

    /// Every job held, running, waiting or ended since the last start, in start order.
    /// Output: their views.
    pub(super) fn status(&self) -> Vec<JobView> {
        let held = self.lock();
        assert!(held.len() <= 2 * (JOBS_RUNNING_MAX + JOBS_WAITING_MAX));
        held.iter().map(|current| current.job.view()).collect()
    }

    /// Lets go every waiting job whose turn came, once a job ended or left the waiting list.
    pub(super) fn admit(&self) {
        admit(&mut self.lock());
    }

    /// Asks a live job to stop: a running one between files, a waiting one at once, which
    /// lets its task go and ends it cancelled while waiting, so the jobs behind it may start.
    /// Input: its id. Output: whether that job was live; its view then reads cancelled, or
    /// done when its folder had landed; or `delete_cannot_stop` for a delete that committed to
    /// its first Trash move, or `setup_cannot_stop` for a New Project whose record started.
    pub(super) fn stop(&self, id: &RequestId) -> Result<bool, EngineError> {
        let mut held = self.lock();
        let Some(current) = held
            .iter_mut()
            .find(|current| current.job.id() == id && current.live())
        else {
            return Ok(false);
        };
        if !current.job.stop() {
            return match current.job.kind() {
                JobKind::Delete => Err(failure("delete_cannot_stop")),
                JobKind::Setup => Err(failure("setup_cannot_stop")),
                kind => panic!("a {kind:?} job always takes a stop"),
            };
        }
        if let Some(turn) = current.turn.take() {
            current.job.end(Err(failure(current.job.interrupted())));
            drop(turn);
            assert!(!current.live());
            admit(&mut held);
        }
        Ok(true)
    }

    /// Reads the job an engine stop cut short, a project move first, then a delete, then the
    /// newest add, else a New Project, so status shows how it ended until the owner dismisses
    /// it or a new job starts. The New Project read comes first whatever is shown, since it
    /// ends every New Project mark an earlier engine process left. Runs at server start, after
    /// the storage start finished any cut move from disk; a job a client started first wins.
    /// Inputs: the engine and the wait. Output: none; a failed read is logged and shows
    /// nothing.
    pub(super) async fn restore(&self, engine: &Engine, wait: Duration) {
        let setup = setup_restored(engine, wait).await;
        let Some((job, real)) = restored(engine, wait).await.or(setup) else {
            return;
        };
        assert!(job.ended());
        let mut held = self.lock();
        if held.is_empty() {
            held.push(Held {
                job: Arc::new(job),
                real,
                task: None,
                turn: None,
            });
        }
    }

    /// Forgets an ended job, so status no longer shows it. Input: its id. Output: the kind of
    /// the ended job forgotten, or None; a live job is never forgotten.
    pub(super) fn dismiss(&self, id: &RequestId) -> Option<JobKind> {
        let mut held = self.lock();
        let at = held
            .iter()
            .position(|current| current.job.id() == id && current.job.ended())?;
        let kind = held.remove(at).job.kind();
        assert!(!held.iter().any(|current| current.job.id() == id));
        Some(kind)
    }

    /// Forgets the delete cut short Home shows for a project once the owner kept the project
    /// from its banner, so Home stops offering it. Input: the project id. Output: whether it
    /// was forgotten.
    pub(super) fn forget_cut_short(&self, project: &str) -> bool {
        let mut held = self.lock();
        let before = held.len();
        held.retain(|current| {
            let cut = current.job.kind() == JobKind::Delete
                && current
                    .job
                    .project()
                    .is_some_and(|id| id.as_str() == project)
                && matches!(current.job.view().state, JobState::CutShort { .. });
            !cut
        });
        assert!(before - held.len() <= 1, "one delete per project");
        before != held.len()
    }

    /// Stops every job as the engine stops, after its clients ended, so none starts another:
    /// asks each to stop and lets every waiting one go, waits up to `wait` in all for their
    /// copies to end between files and their ends to be recorded, then aborts what is left.
    /// Input: the wait.
    pub(super) async fn stop_all(&self, wait: Duration) {
        let taken = std::mem::take(&mut *self.lock());
        assert!(self.lock().is_empty());
        let deadline = tokio::time::Instant::now() + wait;
        let mut tasks = Vec::with_capacity(taken.len());
        for Held {
            job, task, turn, ..
        } in taken
        {
            job.stop();
            drop(turn);
            tasks.extend(task);
        }
        for task in tasks {
            let abort = task.abort_handle();
            if tokio::time::timeout_at(deadline, task).await.is_err() {
                abort.abort();
                eprintln!("intake_job_abandoned_at_stop");
            }
        }
    }

    /// Holds the jobs off for a direct client folder add or project move while it runs, so it
    /// never runs beside a job and no job starts until it ends. Output: the hold, dropped
    /// when the change ends, or `job_busy` while a job is live.
    pub(super) fn direct(&self) -> Result<Direct<'_>, EngineError> {
        let held = self.lock();
        if held.iter().any(Held::live) {
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

    fn lock(&self) -> std::sync::MutexGuard<'_, Vec<Held>> {
        self.held
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// The New Project an engine stop cut short: the read ends every mark an earlier engine
/// process left and gives the newest whose project was never recorded. Inputs: the engine and
/// the wait. Output: the ended job and an empty real folder, never compared, or None when
/// there is none or the read failed, which is logged.
async fn setup_restored(engine: &Engine, wait: Duration) -> Option<(IntakeJob, PathBuf)> {
    let cut = engine.owner_job(Job::HomeSetupCutShort);
    match tokio::time::timeout(wait, cut).await {
        Ok(Ok(Some(found))) => {
            let job = IntakeJob::setup_interrupted(found.request, found.project, found.name);
            Some((job, PathBuf::new()))
        }
        Ok(Ok(None)) => None,
        Ok(Err(e)) => {
            eprintln!("home_new_project_not_restored cause={}", e.0);
            None
        }
        Err(_) => {
            eprintln!("home_new_project_not_restored cause=timeout");
            None
        }
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

/// Runs one job: forgets the ended jobs it replaced, waits for its turn, does the job's work,
/// records how the job ended, then lets go the waiting jobs whose turn came. A turn let go
/// unsent, by a stop while waiting or the engine's stop, ends the job before any work. The
/// claim on the job's id is dropped before the end is recorded, so a new job that may start
/// then never finds the claim held. Inputs: the engine, the authority, the packet and
/// admission of the client that started it, the job, the subject that started it, its
/// claim, the ended jobs it replaced and its turn.
#[allow(clippy::too_many_arguments)]
async fn run(
    engine: Arc<Engine>,
    authority: Arc<Authority>,
    packet: [u8; 37],
    a: Admission,
    job: Arc<IntakeJob>,
    subject: JobSubject,
    claim: Running,
    replaced: Vec<RequestId>,
    turn: oneshot::Receiver<()>,
) {
    assert_eq!(subject.kind(), job.kind());
    assert!(replaced.len() <= 2 * (JOBS_RUNNING_MAX + JOBS_WAITING_MAX));
    let ending = Ending(Arc::clone(&job));
    for old in replaced {
        if let Err(e) = engine
            .owner_job(|reply| Job::HomeJobForget(old.clone(), reply))
            .await
        {
            eprintln!("job_not_forgotten cause={}", e.0);
        }
    }
    let result = match turn.await {
        Ok(()) => Some(worked(&engine, &authority, &packet, &a, &job, &subject).await),
        Err(_) => None,
    };
    drop(claim);
    match result {
        Some(result) => ending.end(result),
        None => drop(ending),
    }
    assert!(job.ended());
    authority.intake.admit();
}

/// The job's work in order. An add: the check, the download, then the add planned in full
/// again right before its copy. A project move, checked at its start: the download of what
/// of the project's folder is only in iCloud, then the move planned in full, which waits
/// for the project's agents before anything moves. A delete, checked at its start: the
/// delete planned in full, which waits for the agents, then moves the folders to the Trash.
/// A New Project, checked at its start: its mark, the New Project planned in full, built
/// and recorded, then its mark ended, which an engine stop leaves for the next start to show.
/// Inputs: as [`run`], the subject by reference. Output: Ok once the change is recorded as
/// applied, else the first step's code.
async fn worked(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    job: &Arc<IntakeJob>,
    subject: &JobSubject,
) -> Result<(), EngineError> {
    if job.kind() == JobKind::Delete {
        job.copying();
        return served(engine, authority, packet, a, job, subject).await;
    }
    if job.kind() == JobKind::Setup {
        job.copying();
        let project = job
            .project()
            .cloned()
            .expect("a New Project job names its project");
        let (id, name) = (job.id().clone(), job.source().to_owned());
        let mark = engine.owner_job(|reply| Job::HomeSetupMark(project.clone(), id, name, reply));
        tokio::time::timeout(engine.timeout, mark)
            .await
            .map_err(|_| failure(UNKNOWN))??;
        let result = served(engine, authority, packet, a, job, subject).await;
        let end = engine.owner_job(|reply| Job::HomeSetupMarkEnd(project, reply));
        match tokio::time::timeout(engine.timeout, end).await {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => eprintln!("home_new_project_mark_not_ended cause={}", e.0),
            Err(_) => eprintln!("home_new_project_mark_not_ended cause=timeout"),
        }
        return result;
    }
    if job.kind() == JobKind::Add {
        check(engine, a, job).await?;
    }
    fetch(authority, job).await?;
    job.copying();
    served(engine, authority, packet, a, job, subject).await
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

/// Checks a New Project before its job starts, writing nothing. Inputs: the engine, the
/// client's admission and the Create action. Output: the folder it would land in, the
/// refusal code, or the unknown-outcome code when storage does not answer in time.
async fn check_setup(
    engine: &Engine,
    a: &Admission,
    action: crate::project_setup::SetupAction,
) -> Result<String, EngineError> {
    let action = Box::new(action);
    let ask = engine.owner_job(|reply| Job::HomeSetupCheck(a.clone(), action, reply));
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

/// Downloads what of the job's folder is only in iCloud, holding the engine's download slot
/// once the downloads of the jobs before it ended, so jobs take turns instead of refusing
/// each other. Input: the authority and the job. Output: Ok once every file is on this Mac,
/// else the download's code, `icloud_download_busy` while a client's own download runs.
async fn fetch(authority: &Arc<Authority>, job: &Arc<IntakeJob>) -> Result<(), EngineError> {
    let _turn = authority.intake.downloads.lock().await;
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
/// admission, the job and its subject. Output: Ok once the change is recorded as applied,
/// else its code.
async fn served(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    job: &IntakeJob,
    subject: &JobSubject,
) -> Result<(), EngineError> {
    let mut result = Err(failure("stale_revision"));
    for _ in 0..REVISION_TRIES {
        result = served_once(engine, authority, packet, a, job, subject).await;
        if !matches!(&result, Err(e) if e.0 == "stale_revision") {
            break;
        }
    }
    result
}

/// Reads the current revision and serves the job's change once, with a fresh owner deadline
/// taken after the download: an AddExistingFolder for an add, a MoveProjectHome for a
/// project move, a DeleteProject with the typed name for a delete, a SetupProject with the
/// owner's answers for a New Project. Inputs: as [`served`]. Output: as [`served`],
/// `stale_revision` included.
async fn served_once(
    engine: &Arc<Engine>,
    authority: &Arc<Authority>,
    packet: &[u8; 37],
    a: &Admission,
    job: &IntakeJob,
    subject: &JobSubject,
) -> Result<(), EngineError> {
    assert_eq!(subject.kind(), job.kind());
    let deadline = Instant::now() + engine.timeout;
    let read = engine.owner_job(|reply| Job::OwnerRead(a.clone(), OwnerRead::ProjectsHome, reply));
    let current = tokio::time::timeout_at(deadline, read)
        .await
        .map_err(|_| failure(UNKNOWN))??;
    let mutation = match (job.kind(), job.project()) {
        (JobKind::Setup, _) => match subject {
            JobSubject::Setup {
                project_id,
                spec,
                setup,
            } => OwnerMutation::SetupProject {
                project_id: project_id.clone(),
                spec: spec.clone(),
                setup: setup.clone(),
            },
            other => panic!("a New Project job runs its own subject, not {other:?}"),
        },
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
    assert_eq!(
        matches!(mutation, OwnerMutation::SetupProject { .. }),
        job.kind() == JobKind::Setup
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
