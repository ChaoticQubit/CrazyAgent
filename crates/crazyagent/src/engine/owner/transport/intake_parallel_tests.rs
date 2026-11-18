use super::*;
use crate::client_controller::OwnerController;
use crate::engine::bootstrap::EngineService;
use crate::project_setup::{Platform, ProjectSetup, ResearchKind};
use crate::projects_home::job::{IntakeJob, JobKind, JobState, JobSubject, JobView};
use crate::projects_home::reinstall::{self, Hint, Kept, Left};
use crate::projects_home::{AddMode, AddOverride, CopyBounds, icloud};
use crate::protocol::{ProjectId, ProjectSpec};
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicUsize, Ordering};

const ENGINE_WAIT: Duration = Duration::from_secs(10);
const JOBS_RUNNING: usize = 3;
const JOBS_WAITING: usize = 8;

/// One engine with its projects home set, an owner client and a folder outside the home for
/// the folders the owner picks, for the rows of several intake jobs at once (DEC-197).
struct Jobs {
    root: tempfile::TempDir,
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
            root,
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

    /// Keeps a reinstall offer at a folder as a done job does, through the engine's storage.
    /// Inputs: the folder and the record. Output: none.
    async fn keep_offer(&self, landed: PathBuf, kept: Kept) {
        self.service
            .owner_runtime()
            .unwrap()
            .engine
            .owner_job(|reply| Job::HomeReinstallKeep(landed, None, kept, reply))
            .await
            .unwrap();
    }

    /// Reads the record kept at a folder straight from the engine's database. Input: the
    /// folder. Output: the record, or None.
    fn offer_at(&self, folder: &Path) -> Option<Kept> {
        let db = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        crate::storage::reinstall::at(&db, folder).unwrap()
    }

    /// Reads the owner ledger's result kept under one request id straight from the engine's
    /// database. Input: the id. Output: the recorded result, or None when the id has no row.
    fn recorded(&self, id: &str) -> Option<OwnerResult> {
        use rusqlite::OptionalExtension;
        let db = rusqlite::Connection::open(self.root.path().join("engine.db")).unwrap();
        let bytes: Option<Vec<u8>> = db
            .query_row(
                "SELECT result FROM owner_operations \
                 WHERE origin = 'native_owner' AND operation_id = ?1",
                [id],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        bytes.map(|bytes| serde_json::from_slice(&bytes).unwrap())
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
            let jobs = self.owner.job_status().await.unwrap();
            let found = jobs.into_iter().find(|job| job.id.as_str() == id);
            if let Some(job) = found.filter(|job| job.state.over()) {
                return job;
            }
            assert!(std::time::Instant::now() < deadline, "job {id} never ended");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// Reads the add `id` of `path` until it has ended by sending its start again, which
    /// answers the job's view and is no status read, so no client has read the end. Inputs:
    /// the id, the folder and the mode. Output: its ended view.
    async fn ended_unread(&mut self, id: &str, path: &str, mode: AddMode) -> JobView {
        let deadline = std::time::Instant::now() + ENGINE_WAIT * 3;
        loop {
            let view = self.add(id, path, mode).await.unwrap();
            assert_eq!(view.id.as_str(), id, "a resend answers its own job");
            if view.state.over() {
                return view;
            }
            assert!(std::time::Instant::now() < deadline, "job {id} never ended");
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    /// The ids status lists, in start order. Output: the ids.
    async fn listed(&mut self) -> Vec<String> {
        let jobs = self.owner.job_status().await.unwrap();
        jobs.iter().map(|job| job.id.as_str().to_owned()).collect()
    }

    /// The engine's owner authority, for a row that holds what a client change holds.
    fn authority(&self) -> Arc<Authority> {
        Arc::clone(&self.service.owner_runtime().unwrap().authority)
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

    /// Stops the engine, lets `cut` change its database as an engine stop left it, then starts
    /// the engine again on the same folder and attaches the owner. Input: the change. Output:
    /// the engine started again.
    async fn restarted(self, cut: impl FnOnce(&rusqlite::Connection)) -> Self {
        let Jobs {
            root,
            home,
            outside,
            mut service,
            _stop: stop,
            owner,
        } = self;
        drop(owner);
        service.shutdown().await.unwrap();
        let db = rusqlite::Connection::open(root.path().join("engine.db")).unwrap();
        cut(&db);
        drop(db);
        let service = EngineService::start_with_settings_sources(
            root.path(),
            "e.sock",
            &root.path().join("global.toml"),
            &root.path().join("project.toml"),
            &stop.subscribe(),
        )
        .await
        .unwrap();
        let owner = OwnerController::attach(
            root.path(),
            crate::engine::owner::OwnerClientKind::Tui,
            ENGINE_WAIT,
        )
        .await
        .unwrap();
        Self {
            root,
            home,
            outside,
            service,
            _stop: stop,
            owner,
        }
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
        assert_eq!(
            jobs.recorded(&format!("add-{n}")),
            Some(OwnerResult::Applied),
            "add-{n} keeps one ledger row, the applied one"
        );
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

/// DEC-197: the adds an engine stop cut while they ran side by side all come back after the
/// next engine start, each its own ended job in start order with nothing moved, and a dismiss
/// of one leaves the other, past another start too.
#[tokio::test(flavor = "multi_thread")]
async fn every_add_an_engine_stop_cut_comes_back_ended_after_the_next_start() {
    let jobs = Jobs::start().await;
    let shop = jobs.folder("shop", "shop\n");
    let garden = jobs.folder("garden", "garden\n");
    let home = jobs.home();
    let mut jobs = jobs
        .restarted(|db| {
            for (request, source) in [("add-shop", &shop), ("add-garden", &garden)] {
                let name = Path::new(source).file_name().unwrap();
                db.execute(
                    "INSERT INTO projects_home_add(request, owner, source, destination, mode)
                     VALUES (?1, 'owner', ?2, ?3, 'move')",
                    rusqlite::params![request, source, home.join(name).to_str().unwrap()],
                )
                .unwrap();
            }
        })
        .await;
    let listed = |status: &[JobView]| -> Vec<String> {
        status
            .iter()
            .map(|job| job.id.as_str().to_owned())
            .collect()
    };
    let status = jobs.owner.job_status().await.unwrap();
    assert_eq!(listed(&status), ["add-shop", "add-garden"]);
    let cut = JobState::Failed {
        code: "home_add_interrupted".into(),
        phase: crate::projects_home::job::Phase::Copying,
    };
    assert!(status.iter().all(|job| job.state == cut), "{status:?}");
    assert_eq!(note(Path::new(&shop)), "shop\n");
    assert_eq!(note(Path::new(&garden)), "garden\n");

    let dismissed = jobs
        .owner
        .job_dismiss(RequestId::new("add-shop").unwrap())
        .await
        .unwrap();
    assert!(dismissed);
    let status = jobs.owner.job_status().await.unwrap();
    assert_eq!(listed(&status), ["add-garden"]);
    let mut jobs = jobs.restarted(|_| {}).await;
    let status = jobs.owner.job_status().await.unwrap();
    assert_eq!(
        listed(&status),
        ["add-garden"],
        "the dismiss reached storage"
    );
    jobs.stop().await;
}

/// #380 F2: a job that ended before any client read it stays listed past another start, so a
/// client still finds its end; once a status read listed the ends, the next start drops them.
#[tokio::test(flavor = "multi_thread")]
async fn an_end_no_client_read_stays_listed_past_another_start() {
    let mut jobs = Jobs::start().await;
    let alpha = jobs.folder("alpha", "a");
    let delta = jobs.folder("delta", "d");
    let echo = jobs.folder("echo", "e");
    jobs.add("add-alpha", &alpha, AddMode::Copy).await.unwrap();
    let alpha_end = jobs.ended_unread("add-alpha", &alpha, AddMode::Copy).await;
    assert_eq!(alpha_end.state, JobState::Done);
    jobs.add("add-delta", &delta, AddMode::Copy).await.unwrap();
    let listed = jobs.listed().await;
    assert!(
        listed.iter().any(|id| id == "add-alpha"),
        "an end no client read stays listed past a start: {listed:?}"
    );
    assert_eq!(jobs.ended("add-delta").await.state, JobState::Done);
    jobs.add("add-echo", &echo, AddMode::Copy).await.unwrap();
    let listed = jobs.listed().await;
    assert!(
        !listed
            .iter()
            .any(|id| id == "add-alpha" || id == "add-delta"),
        "a start drops the ends a status read listed: {listed:?}"
    );
    assert_eq!(jobs.ended("add-echo").await.state, JobState::Done);
    jobs.stop().await;
}

/// #380 F2: an add that asks the owner stays listed past other starts after a status read
/// listed it, so its question is never wiped; the answer, a start of the same folder with
/// the owner's override, takes its place.
#[tokio::test(flavor = "multi_thread")]
async fn a_question_stays_listed_past_other_starts_until_its_answer_starts() {
    let mut jobs = Jobs::start().await;
    let linked = jobs.folder("linked", "l");
    std::fs::write(
        Path::new(&linked).join(".git"),
        "gitdir: /nowhere/.git/worktrees/linked\n",
    )
    .unwrap();
    let other = jobs.folder("other", "o");
    jobs.add("add-linked", &linked, AddMode::Copy)
        .await
        .unwrap();
    let asked = jobs.ended("add-linked").await;
    let question = JobState::NeedsAnswer {
        code: "home_copy_linked_checkout".into(),
    };
    assert_eq!(asked.state, question);
    jobs.add("add-other", &other, AddMode::Copy).await.unwrap();
    let listed = jobs.listed().await;
    assert!(
        listed.iter().any(|id| id == "add-linked"),
        "another start keeps the question: {listed:?}"
    );
    assert_eq!(jobs.ended("add-other").await.state, JobState::Done);
    let listed = jobs.listed().await;
    assert!(
        listed.iter().any(|id| id == "add-linked"),
        "the question outlives the other job's end: {listed:?}"
    );
    jobs.owner
        .job_start(
            RequestId::new("add-linked-anyway").unwrap(),
            JobSubject::Add {
                path: linked.clone(),
            },
            AddMode::Copy,
            vec![AddOverride::LinkedCheckout],
        )
        .await
        .unwrap();
    let listed = jobs.listed().await;
    assert!(
        !listed.iter().any(|id| id == "add-linked"),
        "the answer takes the question's place: {listed:?}"
    );
    assert!(listed.iter().any(|id| id == "add-linked-anyway"));
    jobs.ended("add-linked-anyway").await;
    jobs.stop().await;
}

/// A job under an id the owner ledger already holds for a different change still ends with
/// `operation_identity_conflict`: only a plan refused as stale, which records nothing, plans
/// again under the job's id. The job's folder is not added and the first change's row stays.
/// The start waits out `job_busy` while the client's add still ends after its reply.
#[tokio::test(flavor = "multi_thread")]
async fn a_job_under_an_id_kept_for_a_different_change_ends_with_the_identity_conflict() {
    let mut jobs = Jobs::start().await;
    let first = jobs.folder("first", "1");
    let second = jobs.folder("second", "2");
    let read = jobs.owner.read(OwnerRead::ProjectsHome).await.unwrap();
    let request = OwnerRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("add-same").unwrap(),
        expected_revision: read.revision,
        operation: OwnerOperation::Apply {
            mutation: OwnerMutation::AddExistingFolder {
                source: first,
                mode: AddMode::Copy,
                overrides: Vec::new(),
            },
        },
    };
    let reply = jobs
        .owner
        .owner_client()
        .unwrap()
        .request(request)
        .await
        .unwrap();
    assert!(
        matches!(&reply.response, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied),
        "the client's add is recorded under the id: {:?}",
        reply.response
    );
    let deadline = std::time::Instant::now() + ENGINE_WAIT;
    let started = loop {
        match jobs.add("add-same", &second, AddMode::Copy).await {
            Err(e) if e.0 == "job_busy" && std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            started => break started,
        }
    };
    started.expect("the job starts once the client's add has ended");
    let ended = jobs.ended("add-same").await.state;
    assert!(
        matches!(&ended, JobState::Failed { code, .. } if code == "operation_identity_conflict"),
        "a different change under a kept id is refused: {ended:?}"
    );
    assert!(!jobs.home().join("second").exists());
    assert_eq!(note(&jobs.home().join("first")), "1");
    assert_eq!(jobs.recorded("add-same"), Some(OwnerResult::Applied));
    jobs.stop().await;
}

/// #380 F3: with three jobs running, eight waiting and every client-change place held, one
/// more start answers `job_busy` and never trips the engine's bound on running changes; once
/// the places and storage go free, every job ends done.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_past_the_waiting_list_is_busy_while_client_changes_hold_their_places() {
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
    let authority = jobs.authority();
    let places: Vec<_> = (0..DEFERRED_MAX)
        .map(|n| {
            let id = RequestId::new(format!("client-change-{n}")).unwrap();
            super::projects_home::Running::claim(&authority, "owner", &id)
                .expect("a client change takes a free place")
        })
        .collect();
    let past = jobs
        .add("add-past", &folders[total], AddMode::Copy)
        .await
        .expect_err("a start past the waiting list is refused");
    assert_eq!(past.0, "job_busy");
    drop(places);
    release.send(()).unwrap();
    for n in 0..total {
        assert_eq!(jobs.ended(&format!("add-{n}")).await.state, JobState::Done);
    }
    assert!(!jobs.home().join(format!("folder-{total}")).exists());
    jobs.stop().await;
}

/// A download that runs until its stop is asked, for at most five seconds. Inputs: the
/// download, read for its stop. Output: `home_icloud_not_downloaded` once the stop reached it,
/// else Ok with no file brought down.
fn pull_until_stopped(
    _: &str,
    download: &icloud::Download,
    _: CopyBounds,
) -> Result<u64, EngineError> {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while std::time::Instant::now() < deadline {
        if download.cancelled() {
            return Err(EngineError("home_icloud_not_downloaded".into()));
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    Ok(0)
}

/// #380 F4: a stop asked while an add downloads from iCloud reaches the very download the add
/// runs, so the download ends at its next check and the add with it; the paced download's own
/// checks are fix8's cancel rows in `projects_home::icloud::tests`.
#[tokio::test(flavor = "multi_thread")]
async fn a_stop_reaches_the_download_an_add_runs() {
    let jobs = Jobs::start().await;
    let authority = jobs.authority();
    let job = Arc::new(IntakeJob::new(
        RequestId::new("add-site").unwrap(),
        JobKind::Add,
        None,
        jobs.folder("site", "s"),
        AddMode::Move,
        Vec::new(),
    ));
    let fetching = tokio::spawn({
        let authority = Arc::clone(&authority);
        let job = Arc::clone(&job);
        async move { super::intake::fetch_with(&authority, &job, pull_until_stopped).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    let asked = std::time::Instant::now();
    assert!(job.stop(), "the stop reaches the running add");
    let fetched = fetching.await.unwrap();
    assert!(
        asked.elapsed() < Duration::from_secs(2),
        "the stop never reached the add's download"
    );
    assert_eq!(fetched.unwrap_err().0, "home_icloud_not_downloaded");
    jobs.stop().await;
}

static PULLS_IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
static PULLS_IN_FLIGHT_MOST: AtomicUsize = AtomicUsize::new(0);

/// A download that counts itself in flight and waits up to two seconds for a second one, so
/// two downloads that overlap in time both see two in flight. Inputs: unused. Output: Ok with
/// no file brought down.
fn overlapping_pull(_: &str, _: &icloud::Download, _: CopyBounds) -> Result<u64, EngineError> {
    let now = PULLS_IN_FLIGHT.fetch_add(1, Ordering::SeqCst) + 1;
    PULLS_IN_FLIGHT_MOST.fetch_max(now, Ordering::SeqCst);
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while PULLS_IN_FLIGHT_MOST.load(Ordering::SeqCst) < 2 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    PULLS_IN_FLIGHT_MOST.fetch_max(PULLS_IN_FLIGHT.load(Ordering::SeqCst), Ordering::SeqCst);
    let before = PULLS_IN_FLIGHT.fetch_sub(1, Ordering::SeqCst);
    assert!(before >= 1);
    Ok(0)
}

/// V243-DOWNLOADS-SERIAL: two running adds download their iCloud files side by side through
/// the engine's shared paced downloads (DEC-197), so one add's download never waits for the
/// end of another's.
#[tokio::test(flavor = "multi_thread")]
async fn two_running_adds_download_from_icloud_side_by_side() {
    let jobs = Jobs::start().await;
    let authority = jobs.authority();
    let job = |id: &str, path: String| {
        Arc::new(IntakeJob::new(
            RequestId::new(id).unwrap(),
            JobKind::Add,
            None,
            path,
            AddMode::Move,
            Vec::new(),
        ))
    };
    let one = job("pull-one", jobs.folder("one", "1"));
    let two = job("pull-two", jobs.folder("two", "2"));
    let (first, second) = tokio::join!(
        super::intake::fetch_with(&authority, &one, overlapping_pull),
        super::intake::fetch_with(&authority, &two, overlapping_pull),
    );
    first.unwrap();
    second.unwrap();
    assert_eq!(PULLS_IN_FLIGHT.load(Ordering::SeqCst), 0);
    assert_eq!(
        PULLS_IN_FLIGHT_MOST.load(Ordering::SeqCst),
        2,
        "the second add's download waited for the first one's end"
    );
    jobs.stop().await;
}

/// #380 F3: a waiting job stopped while its task still waits on storage keeps its claim on
/// its id, so eleven claims stay held with ten jobs live. With every client-change place held
/// too, the next start answers `job_busy` and never trips the engine's bound on running
/// changes.
#[tokio::test(flavor = "multi_thread")]
async fn a_start_counts_the_claim_a_stopped_waiting_job_still_holds() {
    let mut jobs = Jobs::start().await;
    let linked = jobs.folder("linked", "l");
    std::fs::write(
        Path::new(&linked).join(".git"),
        "gitdir: /nowhere/.git/worktrees/linked\n",
    )
    .unwrap();
    jobs.add("add-linked", &linked, AddMode::Copy)
        .await
        .unwrap();
    assert!(matches!(
        jobs.ended("add-linked").await.state,
        JobState::NeedsAnswer { .. }
    ));
    let total = JOBS_RUNNING + JOBS_WAITING;
    let folders: Vec<String> = (0..total)
        .map(|n| jobs.folder(&format!("folder-{n}"), &n.to_string()))
        .collect();
    let release = jobs.hold();
    for (n, folder) in folders.iter().take(JOBS_RUNNING).enumerate() {
        jobs.add(&format!("add-{n}"), folder, AddMode::Copy)
            .await
            .unwrap();
    }
    let answer = jobs
        .owner
        .job_start(
            RequestId::new("add-linked-anyway").unwrap(),
            JobSubject::Add {
                path: linked.clone(),
            },
            AddMode::Copy,
            vec![AddOverride::LinkedCheckout],
        )
        .await
        .unwrap();
    assert!(
        matches!(answer.state, JobState::Waiting { .. }),
        "the answer waits behind three running adds: {:?}",
        answer.state
    );
    for (n, folder) in folders
        .iter()
        .enumerate()
        .skip(JOBS_RUNNING)
        .take(JOBS_WAITING - 1)
    {
        jobs.add(&format!("add-{n}"), folder, AddMode::Copy)
            .await
            .unwrap();
    }
    let stopped = jobs
        .owner
        .job_stop(RequestId::new("add-linked-anyway").unwrap())
        .await
        .unwrap();
    assert!(stopped, "the stop reaches the waiting answer");
    let authority = jobs.authority();
    let places: Vec<_> = (0..DEFERRED_MAX)
        .map(|n| {
            let id = RequestId::new(format!("client-change-{n}")).unwrap();
            super::projects_home::Running::claim(&authority, "owner", &id)
                .expect("a client change takes a free place")
        })
        .collect();
    let past = jobs.folder("past", "p");
    let refused = jobs
        .add("add-past", &past, AddMode::Copy)
        .await
        .expect_err("a start while every claim is held is refused");
    assert_eq!(refused.0, "job_busy");
    drop(places);
    release.send(()).unwrap();
    for n in 0..total - 1 {
        assert_eq!(jobs.ended(&format!("add-{n}")).await.state, JobState::Done);
    }
    assert!(matches!(
        jobs.ended("add-linked-anyway").await.state,
        JobState::Cancelled { .. }
    ));
    assert!(!jobs.home().join("past").exists());
    jobs.stop().await;
}

/// DEC-198 slice 3: a done add keeps the offer for the folder it landed in, so an add that
/// left nothing in iCloud clears an offer kept there before; the record follows the job's
/// own landed folder, not one worked out again from its source.
#[tokio::test(flavor = "multi_thread")]
async fn a_done_add_that_left_nothing_clears_the_offer_kept_at_its_landed_folder() {
    let mut jobs = Jobs::start().await;
    let source = jobs.folder("garden", "seeds");
    let landed = jobs.home().join("garden");
    let kept = Kept {
        left: vec![Left {
            path: "node_modules".into(),
            hint: None,
        }],
        more: 0,
    };
    jobs.keep_offer(landed.clone(), kept.clone()).await;
    assert_eq!(jobs.offer_at(&landed), Some(kept));
    jobs.add("add-garden", &source, AddMode::Copy)
        .await
        .unwrap();
    assert_eq!(jobs.ended("add-garden").await.state, JobState::Done);
    assert_eq!(note(&landed), "seeds");
    assert_eq!(jobs.offer_at(&landed), None);
    jobs.stop().await;
}

/// DEC-198 slice 3: an offer kept at a project's folder reads by the project's id, with the
/// draft the master gets, until the owner dismisses it; an unknown project has none, a
/// second dismiss finds none, and removing the project forgets an offer kept again.
#[tokio::test(flavor = "multi_thread")]
async fn a_projects_offer_reads_by_its_id_until_dismissed_and_goes_with_the_project() {
    let mut jobs = Jobs::start().await;
    let project = ProjectId::new("reading").unwrap();
    let (spec, setup) = reading_list();
    let set = jobs
        .owner
        .setup_project(project.clone(), spec, setup, 2)
        .await
        .unwrap();
    assert!(
        matches!(&set, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied),
        "{set:?}"
    );
    let folder = jobs.home().join("reading-list");
    assert!(folder.is_dir());
    let kept = Kept {
        left: vec![Left {
            path: "web/node_modules".into(),
            hint: Some(Hint::NpmCi),
        }],
        more: 1,
    };
    jobs.keep_offer(folder.clone(), kept.clone()).await;
    let read = jobs.owner.reinstall_offer(project.clone()).await.unwrap();
    assert_eq!(read, Some(reinstall::offer(&kept)));
    let nobody = ProjectId::new("nobody").unwrap();
    assert_eq!(jobs.owner.reinstall_offer(nobody).await.unwrap(), None);
    assert!(jobs.owner.reinstall_dismiss(project.clone()).await.unwrap());
    assert_eq!(
        jobs.owner.reinstall_offer(project.clone()).await.unwrap(),
        None
    );
    assert!(!jobs.owner.reinstall_dismiss(project.clone()).await.unwrap());
    jobs.keep_offer(folder.clone(), kept).await;
    let removed = jobs.owner.remove_project(project, 3).await.unwrap();
    assert!(
        matches!(&removed, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied),
        "{removed:?}"
    );
    assert_eq!(jobs.offer_at(&folder), None);
    jobs.stop().await;
}

/// The Reading list project's name and setup, a website with market research and no pool.
/// Output: the spec and the setup.
fn reading_list() -> (ProjectSpec, ProjectSetup) {
    let setup = ProjectSetup {
        about: "Tracks reading lists.".into(),
        tech_stack: vec!["rust".into()],
        platforms: vec![Platform::Website],
        research: vec![ResearchKind::MarketResearch],
        pool: None,
        team: None,
        documents: vec![],
    };
    let spec = ProjectSpec {
        name: "Reading list".into(),
        repositories: vec![],
    };
    (spec, setup)
}

/// #419 sec F2: an offer left at a path whose folder is gone, as one kept for an add whose
/// folder was deleted outside CrazyAgent before it was set up, is forgotten when a New Project
/// lands at that path, so the new project offers no reinstall of the old folder's folders.
#[tokio::test(flavor = "multi_thread")]
async fn a_new_project_landing_where_a_stale_offer_is_kept_forgets_it() {
    let mut jobs = Jobs::start().await;
    let folder = jobs.home().join("reading-list");
    let kept = Kept {
        left: vec![Left {
            path: "node_modules".into(),
            hint: None,
        }],
        more: 0,
    };
    jobs.keep_offer(folder.clone(), kept.clone()).await;
    assert_eq!(jobs.offer_at(&folder), Some(kept));
    assert!(!folder.exists());
    let project = ProjectId::new("reading").unwrap();
    let (spec, setup) = reading_list();
    let set = jobs
        .owner
        .setup_project(project.clone(), spec, setup, 2)
        .await
        .unwrap();
    assert!(
        matches!(&set, OwnerResponse::Applied { operation } if operation.result == OwnerResult::Applied),
        "{set:?}"
    );
    assert!(folder.is_dir());
    assert_eq!(jobs.offer_at(&folder), None);
    assert_eq!(jobs.owner.reinstall_offer(project).await.unwrap(), None);
    jobs.stop().await;
}
