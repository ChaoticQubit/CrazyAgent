use super::*;
use crate::research::accounts::{Reach, SettingsResolver};
use crate::research::service::ResearchService;
use crate::team::runtime::live_failover_tests::OwnerAccount;
use crate::terminal::FOLDER;
use crate::terminal::tmux::{SOCKET, Tmux, installed};

const TICK: Duration = Duration::from_secs(1);

/// The engine's research service opened on the database a real test engine left after it
/// stopped, as the engine starts it: jobs claimed from that database, pool assignment read
/// from its settings files, every agent in its own tmux pane. Its database carries the
/// owner's engine run and the owner account's proven sign-in, and its custody is the
/// owner's engine's own, which only reads that database and takes a shared lock on the
/// profile for each launch.
pub(crate) struct EngineResearch {
    service: Option<ResearchService>,
    jobs: ResearchJobs,
    host: Arc<dyn Host>,
    tmux: PathBuf,
    base: PathBuf,
}

impl EngineResearch {
    /// Starts research on the engine folder `base` with the owner's account and the pools
    /// its settings files hold. Inputs: the engine folder, its global and project settings
    /// files and the owner's account. Output: the running research, or a panic with the
    /// plain BLOCKED message when tmux or the account's harness is not installed.
    pub(crate) fn on_engine(
        base: &Path,
        global: &Path,
        settings: &Path,
        owner: &OwnerAccount,
    ) -> Self {
        let base = base.canonicalize().unwrap();
        let database = base.join("engine.db");
        let db = rusqlite::Connection::open(&database).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        owner.record_run(&db);
        drop(db);
        let tmux = installed(&base).expect("BLOCKED: tmux must be installed");
        let host: Arc<dyn Host> = Arc::new(Tmux::open(Some(tmux.clone()), &base.join(FOLDER)));
        let (state, projects) = (base.join("research"), base.join("projects"));
        for directory in [&state, &projects] {
            match fs::DirBuilder::new().mode(0o700).create(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => panic!("research folder: {error}"),
            }
            assert!(
                fs::symlink_metadata(directory)
                    .unwrap()
                    .file_type()
                    .is_dir()
            );
        }
        let (harness, executable) = owner.launch();
        let codex = match harness {
            "codex" => executable.clone(),
            _ => PathBuf::from("/usr/bin/true"),
        };
        let mut runner = ResearchRunner::new(&database, &state, &codex, ResearchLimits::default())
            .unwrap()
            .with_settings(global, settings);
        if harness == "claude-code" {
            runner = runner.with_claude(&executable).unwrap();
        }
        let mut runner = runner.with_terminal(host.clone()).unwrap();
        runner.custody = Mutex::new(Ok(owner.custody()));
        let resolver = SettingsResolver::new(global, settings, &projects, &database).unwrap();
        let service = ResearchService::start(
            Arc::new(runner),
            move |id: &str, harness: &str, reach: Reach| resolver.resolve_with(id, harness, reach),
            TICK,
        );
        Self {
            service: Some(service),
            jobs: ResearchJobs::open(&database).unwrap(),
            host,
            tmux,
            base,
        }
    }

    /// Waits until one job is done, failed or cancelled. Inputs: the job and the time
    /// limit. Output: the ended job; it panics with the job's status and error when the
    /// time runs out.
    pub(crate) fn until_ended(&self, id: i64, limit: Duration) -> ResearchJob {
        let deadline = Instant::now() + limit;
        loop {
            let job = self.jobs.get(id).unwrap().expect("the queued job is kept");
            if matches!(
                job.status,
                ResearchStatus::Done | ResearchStatus::Failed | ResearchStatus::Cancelled
            ) {
                return job;
            }
            assert!(
                Instant::now() < deadline,
                "research job {id} is still {}: {:?}",
                job.status.label(),
                job.error
            );
            std::thread::sleep(TICK);
        }
    }
}

impl Drop for EngineResearch {
    fn drop(&mut self) {
        drop(self.service.take());
        let _ = self.host.close(&[]);
        let _ = crate::spawn_gate::status(
            Command::new(&self.tmux)
                .arg("-S")
                .arg(self.base.join(FOLDER).join(SOCKET))
                .arg("kill-server")
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        );
    }
}
