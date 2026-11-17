use super::*;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use crate::projects_home::AddMode;
use crate::projects_home::job::{JobState, JobSubject, JobView};
use std::os::unix::fs::PermissionsExt;

const ENGINE_WAIT: Duration = Duration::from_secs(10);
const JOBS_RUNNING: usize = 3;
const JOBS_WAITING: usize = 8;

/// One engine with its projects home set, an owner client and a folder outside the home for
/// the folders the owner picks, for the rows of several intake jobs at once (DEC-197).
struct Jobs {
    _root: tempfile::TempDir,
    home: tempfile::TempDir,
    outside: tempfile::TempDir,
    service: EngineService,
    _stop: tokio::sync::watch::Sender<bool>,
    owner: OwnerController,
}

impl Jobs {
    /// Starts an engine whose owner waits up to ten seconds, attaches the owner and sets the
    /// projects home. Output: the engine.
    async fn start() -> Self {
        let root = private("ca-jobs-");
        let global = root.path().join("global.toml");
        std::fs::write(
            &global,
            format!("timeout_ms = {}\n", ENGINE_WAIT.as_millis()),
        )
        .unwrap();
        std::fs::set_permissions(&global, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (stop, stopped) = tokio::sync::watch::channel(false);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &global,
            &root.path().join("project.toml"),
            &stopped,
        )
        .await
        .unwrap();
        let home = private("ca-jobs-home-");
        let outside = private("ca-jobs-out-");
        let mut owner = OwnerController::attach(
            root.path(),
            crate::engine::owner::OwnerClientKind::Tui,
            ENGINE_WAIT,
        )
        .await
        .unwrap();
        let set = owner
            .set_projects_home(canonical(home.path()).to_str().unwrap().into(), 1)
            .await
            .unwrap();
        assert!(
            matches!(&set, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied),
            "the projects home is set: {set:?}"
        );
        Self {
            _root: root,
            home,
            outside,
            service,
            _stop: stop,
            owner,
        }
    }

    /// The canonical projects home.
    fn home(&self) -> PathBuf {
        canonical(self.home.path())
    }

    /// Makes a folder outside the home holding one note, as the owner picks it. Inputs: its
    /// path below the outside folder and the note's text. Output: the folder's path text.
    fn folder(&self, below: &str, text: &str) -> String {
        let folder = canonical(self.outside.path()).join(below);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("note.txt"), text).unwrap();
        folder.to_str().unwrap().to_owned()
    }

    /// Holds the engine's storage until the returned sender sends or drops, so every job
    /// that reaches storage meanwhile waits on the test. Output: the sender.
    fn hold(&self) -> oneshot::Sender<()> {
        let (release, held) = oneshot::channel();
        self.service
            .owner_runtime()
            .unwrap()
            .engine
            .storage
            .sender
            .try_send(Job::Hold(held))
            .unwrap();
        release
    }

    /// Starts an add of `path` under the request id `id`. Inputs: the id, the folder and the
    /// mode. Output: the start's answer.
    async fn add(&mut self, id: &str, path: &str, mode: AddMode) -> Result<JobView, EngineError> {
        self.owner
            .job_start(
                RequestId::new(id).unwrap(),
                JobSubject::Add {
                    path: path.to_owned(),
                },
                mode,
                Vec::new(),
            )
            .await
    }

    /// Reads the job `id` until it has ended, within three engine waits. Input: the id.
    /// Output: its ended view.
    async fn ended(&mut self, id: &str) -> JobView {
        let deadline = std::time::Instant::now() + ENGINE_WAIT * 3;
        loop {
            let shown = self.owner.job_status().await.unwrap();
            if let Some(job) = shown.filter(|job| job.id.as_str() == id && over(&job.state)) {
                return job;
            }
            assert!(std::time::Instant::now() < deadline, "job {id} never ended");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The names in the home that a copy stages under, left behind by a job.
    fn staging(&self) -> Vec<String> {
        std::fs::read_dir(self.home())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(".crazyagent-adding-"))
            .collect()
    }

    async fn stop(mut self) {
        drop(self.owner);
        self.service.shutdown().await.unwrap();
    }
}

/// A private folder under the real /tmp. Input: its name's prefix. Output: the folder.
fn private(prefix: &str) -> tempfile::TempDir {
    let folder = tempfile::Builder::new()
        .prefix(prefix)
        .tempdir_in(std::fs::canonicalize("/tmp").unwrap())
        .unwrap();
    std::fs::set_permissions(folder.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    folder
}

fn canonical(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap()
}

/// Whether a job in this state has ended. Input: the state. Output: the answer.
fn over(state: &JobState) -> bool {
    matches!(
        state,
        JobState::Done
            | JobState::Failed { .. }
            | JobState::Cancelled { .. }
            | JobState::NeedsAnswer { .. }
            | JobState::CutShort { .. }
    )
}

/// The note a folder holds. Input: the folder. Output: its text.
fn note(folder: &Path) -> String {
    std::fs::read_to_string(folder.join("note.txt")).unwrap()
}

/// DEC-197: two folders picked one after the other move into the home side by side. The
/// second start, made while the first job still waits on storage, is a job of its own, not
/// `job_busy`, and both end done, each folder in the home and gone from where it was.
#[tokio::test(flavor = "multi_thread")]
async fn two_folders_move_into_the_home_together() {
    let mut jobs = Jobs::start().await;
    let (alpha, beta) = (jobs.folder("alpha", "a"), jobs.folder("beta", "b"));
    let release = jobs.hold();
    let first = jobs.add("add-alpha", &alpha, AddMode::Move).await.unwrap();
    let second = jobs
        .add("add-beta", &beta, AddMode::Move)
        .await
        .expect("a second folder starts a job of its own while the first runs");
    assert_eq!(first.id.as_str(), "add-alpha");
    assert_eq!(second.id.as_str(), "add-beta");
    release.send(()).unwrap();
    assert_eq!(jobs.ended("add-alpha").await.state, JobState::Done);
    assert_eq!(jobs.ended("add-beta").await.state, JobState::Done);
    assert_eq!(note(&jobs.home().join("alpha")), "a");
    assert_eq!(note(&jobs.home().join("beta")), "b");
    assert!(!Path::new(&alpha).exists(), "a move leaves no source");
    assert!(!Path::new(&beta).exists(), "a move leaves no source");
    jobs.stop().await;
}

/// DEC-197: while a folder is being added, a start for the same folder under another id, for
/// a folder inside it and for the folder around it is refused `job_same_folder` with no job of
/// its own; a resend of the running start shows the running job, which ends done.
#[tokio::test(flavor = "multi_thread")]
async fn the_same_folder_or_one_inside_or_around_it_is_refused_while_it_is_added() {
    let mut jobs = Jobs::start().await;
    let site = jobs.folder("group/site", "s");
    let inside = jobs.folder("group/site/inner", "i");
    let around = Path::new(&site)
        .parent()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let release = jobs.hold();
    jobs.add("add-site", &site, AddMode::Copy).await.unwrap();
    for (id, path) in [("again", &site), ("inside", &inside), ("around", &around)] {
        let refused = jobs
            .add(id, path, AddMode::Copy)
            .await
            .expect_err("a folder already being added is refused");
        assert_eq!(refused.0, "job_same_folder", "start {id}");
    }
    let resent = jobs.add("add-site", &site, AddMode::Copy).await.unwrap();
    assert_eq!(
        resent.id.as_str(),
        "add-site",
        "a resend shows the running job"
    );
    release.send(()).unwrap();
    assert_eq!(jobs.ended("add-site").await.state, JobState::Done);
    assert_eq!(note(&jobs.home().join("site")), "s");
    assert_eq!(note(&jobs.home().join("site").join("inner")), "i");
    jobs.stop().await;
}

/// DEC-197: two folders of one name from different places. The second job waits for the
/// first with no error; once the first lands it ends failed `home_destination_exists`, its
/// own folder untouched, with no half-made folder or staging folder in the home.
#[tokio::test(flavor = "multi_thread")]
async fn a_second_folder_of_the_same_name_waits_then_finds_the_name_taken() {
    let mut jobs = Jobs::start().await;
    let (one, two) = (jobs.folder("one/site", "1"), jobs.folder("two/site", "2"));
    let release = jobs.hold();
    jobs.add("add-one", &one, AddMode::Copy).await.unwrap();
    jobs.add("add-two", &two, AddMode::Copy)
        .await
        .expect("a folder whose name another job takes waits instead of being refused");
    release.send(()).unwrap();
    assert_eq!(jobs.ended("add-one").await.state, JobState::Done);
    let second = jobs.ended("add-two").await;
    assert!(
        matches!(&second.state, JobState::Failed { code, .. } if code == "home_destination_exists"),
        "the second finds the name taken: {:?}",
        second.state
    );
    assert_eq!(
        note(&jobs.home().join("site")),
        "1",
        "the first folder landed whole"
    );
    assert_eq!(note(Path::new(&two)), "2", "the second folder is untouched");
    assert_eq!(jobs.staging(), Vec::<String>::new(), "no half-made folder");
    jobs.stop().await;
}

/// DEC-197: three jobs run and eight more wait; one start past them is refused `job_busy`,
/// and once storage goes on every waiting job starts by itself and ends done.
#[tokio::test(flavor = "multi_thread")]
async fn waiting_jobs_start_by_themselves_and_a_start_past_the_waiting_list_is_busy() {
    let mut jobs = Jobs::start().await;
    let total = JOBS_RUNNING + JOBS_WAITING;
    let folders: Vec<String> = (0..=total)
        .map(|n| jobs.folder(&format!("folder-{n}"), &n.to_string()))
        .collect();
    let release = jobs.hold();
    for (n, folder) in folders.iter().take(total).enumerate() {
        jobs.add(&format!("add-{n}"), folder, AddMode::Copy)
            .await
            .unwrap_or_else(|e| panic!("start {n} of {total} is taken: {}", e.0));
    }
    let past = jobs
        .add("add-past", &folders[total], AddMode::Copy)
        .await
        .expect_err("a start past the waiting list is refused");
    assert_eq!(past.0, "job_busy");
    release.send(()).unwrap();
    for n in 0..total {
        assert_eq!(jobs.ended(&format!("add-{n}")).await.state, JobState::Done);
        let landed = jobs.home().join(format!("folder-{n}"));
        assert_eq!(note(&landed), n.to_string());
    }
    assert!(!jobs.home().join(format!("folder-{total}")).exists());
    jobs.stop().await;
}

/// DEC-197: a stop of one of two running adds ends that one cancelled, its folder where it
/// was and nothing of it in the home, while the other goes on and ends done.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_of_one_job_leaves_the_other_running() {
    let mut jobs = Jobs::start().await;
    let (keep, stop) = (jobs.folder("keep", "k"), jobs.folder("stop", "s"));
    let release = jobs.hold();
    jobs.add("add-keep", &keep, AddMode::Move).await.unwrap();
    jobs.add("add-stop", &stop, AddMode::Move)
        .await
        .expect("a second folder starts a job of its own while the first runs");
    let stopped = jobs
        .owner
        .job_stop(RequestId::new("add-stop").unwrap())
        .await
        .unwrap();
    assert!(stopped, "the stop reaches the running job");
    release.send(()).unwrap();
    let ended = jobs.ended("add-stop").await;
    assert!(
        matches!(ended.state, JobState::Cancelled { .. }),
        "{:?}",
        ended.state
    );
    assert_eq!(jobs.ended("add-keep").await.state, JobState::Done);
    assert_eq!(note(&jobs.home().join("keep")), "k");
    assert_eq!(
        note(Path::new(&stop)),
        "s",
        "a stopped move leaves its folder"
    );
    assert!(!jobs.home().join("stop").exists());
    jobs.stop().await;
}
