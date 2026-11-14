use super::*;
use crate::projects_home::COPY_BOUNDS;

fn seconds(count: u64) -> Duration {
    Duration::from_secs(count)
}

#[test]
fn a_download_keeps_waiting_while_files_arrive_and_stops_after_ten_quiet_minutes() {
    let start = Instant::now();
    let mut watch = Watch::new(start, start + DOWNLOAD_TIME_MAX);
    assert_eq!(watch.observe(0, 0, start), Verdict::Wait);
    assert_eq!(watch.observe(5, 0, start + seconds(300)), Verdict::Wait);
    assert_eq!(watch.observe(5, 0, start + seconds(899)), Verdict::Wait);
    assert_eq!(watch.observe(6, 0, start + seconds(1199)), Verdict::Wait);
    assert_eq!(watch.observe(6, 0, start + seconds(1798)), Verdict::Wait);
    assert_eq!(
        watch.observe(6, 0, start + seconds(1199) + STALL_MAX),
        Verdict::Stalled
    );
}

#[test]
fn a_download_whose_count_drops_back_is_not_progress() {
    let start = Instant::now();
    let mut watch = Watch::new(start, start + DOWNLOAD_TIME_MAX);
    assert_eq!(watch.observe(10, 0, start), Verdict::Wait);
    assert_eq!(watch.observe(4, 0, start + seconds(60)), Verdict::Wait);
    assert_eq!(watch.observe(9, 0, start + STALL_MAX), Verdict::Stalled);
}

#[test]
fn a_download_still_making_progress_stops_at_its_deadline() {
    let start = Instant::now();
    let deadline = start + seconds(100);
    let mut watch = Watch::new(start, deadline);
    assert_eq!(watch.observe(1, 0, start + seconds(99)), Verdict::Wait);
    assert_eq!(watch.observe(2, 0, deadline), Verdict::TimedOut);
}

#[test]
fn a_large_file_left_gets_time_for_its_bytes_before_the_wait_reads_stalled() {
    let start = Instant::now();
    let mut watch = Watch::new(start, start + DOWNLOAD_TIME_MAX);
    let large = 4096 * STALL_FLOOR_BYTES_PER_SEC;
    assert_eq!(watch.observe(3, large, start), Verdict::Wait);
    assert_eq!(watch.observe(3, large, start + STALL_MAX), Verdict::Wait);
    assert_eq!(
        watch.observe(3, large, start + STALL_MAX + seconds(4095)),
        Verdict::Wait
    );
    assert_eq!(
        watch.observe(3, large, start + STALL_MAX + seconds(4096)),
        Verdict::Stalled
    );
    let mut small = Watch::new(start, start + DOWNLOAD_TIME_MAX);
    assert_eq!(
        small.observe(0, STALL_FLOOR_BYTES_PER_SEC - 1, start + STALL_MAX),
        Verdict::Stalled
    );
}

#[test]
fn a_cancel_ends_the_pause_within_one_check() {
    let download = Arc::new(Download::default());
    let canceller = {
        let download = Arc::clone(&download);
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            download.cancel();
        })
    };
    let started = Instant::now();
    let stopped = pause(&download, seconds(30)).unwrap_err();
    canceller.join().unwrap();
    assert_eq!(stopped.0, "home_icloud_not_downloaded");
    assert!(started.elapsed() < Duration::from_millis(150) + CANCEL_EVERY * 3);
    assert!(pause(&Download::default(), Duration::from_millis(10)).is_ok());
}

#[test]
fn one_download_runs_at_a_time_and_a_dropped_claim_cancels_it_and_frees_the_slot() {
    let downloads = Downloads::default();
    assert_eq!(downloads.progress(), None);
    assert!(!downloads.cancel());
    let claim = downloads.claim().unwrap();
    let download = claim.download();
    download.record(8, 3);
    assert_eq!(downloads.progress(), Some(Progress { total: 8, done: 3 }));
    assert_eq!(downloads.claim().unwrap_err().0, "icloud_download_busy");
    assert!(!download.cancelled());
    drop(claim);
    assert!(download.cancelled());
    assert_eq!(downloads.progress(), None);
    let again = downloads.claim().unwrap();
    assert!(downloads.cancel());
    assert!(again.download().cancelled());
}

#[cfg(unix)]
#[test]
fn brctl_runs_by_absolute_path_with_each_placeholder_as_its_own_argument() {
    let batch = vec![
        PathBuf::from("/Users/mira/Desktop/shop/a b.txt"),
        PathBuf::from("/Users/mira/Desktop/shop/$(rm -rf ~).txt"),
    ];
    let command = brctl(&batch);
    assert_eq!(command.get_program(), "/usr/bin/brctl");
    let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
    assert_eq!(args.len(), 3);
    assert_eq!(args[0], "download");
    assert_eq!(args[1], batch[0].as_os_str());
    assert_eq!(args[2], batch[1].as_os_str());
    assert_eq!(command.get_envs().count(), 0);
}

/// Stands in for iCloud: `count` placeholders under /icloud, each landing the moment its ask
/// ends, with every ask taking `ask_time`.
struct FakeIcloud {
    paths: Vec<PathBuf>,
    landed: std::sync::atomic::AtomicUsize,
    asks: std::sync::atomic::AtomicUsize,
    ask_time: Duration,
}

impl FakeIcloud {
    fn new(count: usize, ask_time: Duration) -> Self {
        assert!(count > 0);
        Self {
            paths: (0..count)
                .map(|n| PathBuf::from(format!("/icloud/f{n:05}")))
                .collect(),
            landed: Default::default(),
            asks: Default::default(),
            ask_time,
        }
    }

    fn ask(&self, batch: &[PathBuf]) -> Result<(), EngineError> {
        assert!(!batch.is_empty() && batch.len() <= BATCH_PATHS_MAX);
        self.asks.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(self.ask_time);
        let landed = self.landed.fetch_add(batch.len(), Ordering::SeqCst);
        assert_eq!(
            batch.first(),
            self.paths.get(landed),
            "batches are asked in order"
        );
        Ok(())
    }

    fn list(&self) -> Result<Placeholders, EngineError> {
        let landed = self.landed.load(Ordering::SeqCst);
        assert!(landed <= self.paths.len());
        Ok(Placeholders {
            paths: self.paths[landed..].to_vec(),
            largest_bytes: 0,
        })
    }
}

#[test]
fn files_that_land_while_slow_asks_still_go_are_counted_before_the_last_ask_ends() {
    let batches = 6;
    let icloud = FakeIcloud::new(BATCH_PATHS_MAX * batches, Duration::from_millis(80));
    let download = Download::default();
    let seen = AtomicU64::new(0);
    let ask = |batch: &[PathBuf], _stop: &dyn Fn() -> bool| {
        seen.fetch_max(download.progress().done, Ordering::SeqCst);
        icloud.ask(batch)
    };
    let list = |_: &Path| icloud.list();
    let poll = Duration::from_millis(10);
    let files = run(
        Path::new("/icloud"),
        &download,
        &ask,
        &list,
        COPY_BOUNDS,
        poll,
    )
    .unwrap();
    let total = (BATCH_PATHS_MAX * batches) as u64;
    assert_eq!(files, total);
    assert_eq!(download.progress(), Progress { total, done: total });
    assert_eq!(icloud.asks.load(Ordering::SeqCst), batches);
    let counted = seen.load(Ordering::SeqCst);
    assert!(
        counted >= BATCH_PATHS_MAX as u64,
        "progress read {counted} while later asks still waited"
    );
    assert!(counted < total);
}

#[test]
fn a_failed_ask_ends_the_download_with_its_code_and_asks_nothing_more() {
    let icloud = FakeIcloud::new(BATCH_PATHS_MAX * 4, Duration::from_millis(20));
    let download = Download::default();
    let ask = |batch: &[PathBuf], _stop: &dyn Fn() -> bool| {
        if icloud.asks.load(Ordering::SeqCst) == 1 {
            icloud.asks.fetch_add(1, Ordering::SeqCst);
            return Err(refused(NOT_DOWNLOADED));
        }
        icloud.ask(batch)
    };
    let list = |_: &Path| icloud.list();
    let started = Instant::now();
    let failed = run(
        Path::new("/icloud"),
        &download,
        &ask,
        &list,
        COPY_BOUNDS,
        Duration::from_millis(10),
    );
    assert_eq!(failed.unwrap_err().0, NOT_DOWNLOADED);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(icloud.asks.load(Ordering::SeqCst), 2);
    assert!(download.progress().done < download.progress().total);
}

#[test]
fn a_cancel_during_slow_asks_stops_the_ask_and_the_download_within_one_check() {
    let icloud = FakeIcloud::new(BATCH_PATHS_MAX * 4, Duration::from_millis(10));
    let download = Download::default();
    let ask = |_: &[PathBuf], stop: &dyn Fn() -> bool| {
        icloud.asks.fetch_add(1, Ordering::SeqCst);
        download.cancel();
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until && !stop() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stop(), "the ask sees the cancel");
        Err(stopped("stopped"))
    };
    let list = |_: &Path| icloud.list();
    let started = Instant::now();
    let cancelled = run(
        Path::new("/icloud"),
        &download,
        &ask,
        &list,
        COPY_BOUNDS,
        Duration::from_millis(10),
    );
    assert_eq!(cancelled.unwrap_err().0, NOT_DOWNLOADED);
    assert!(started.elapsed() < CANCEL_EVERY * 5);
    assert_eq!(icloud.asks.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn a_folder_with_no_placeholders_downloads_nothing_and_never_runs_brctl() {
    let folder = tempfile::tempdir().unwrap();
    fs::create_dir_all(folder.path().join("src/deep")).unwrap();
    fs::write(folder.path().join("src/deep/main.rs"), "fn main() {}\n").unwrap();
    std::os::unix::fs::symlink("/", folder.path().join("root-link")).unwrap();
    assert_eq!(
        placeholders(folder.path(), COPY_BOUNDS).unwrap(),
        Placeholders::default()
    );
    let download = Download::default();
    let path = folder.path().to_str().unwrap();
    assert_eq!(super::download(path, &download).unwrap(), 0);
    assert_eq!(download.progress(), Progress { total: 0, done: 0 });
    for (path, code) in [
        ("relative", "icloud_path_invalid"),
        ("/no/such/folder/here", "icloud_folder_missing"),
        ("/", "icloud_folder_unsafe"),
    ] {
        assert_eq!(
            super::download(path, &download).unwrap_err().0,
            code,
            "{path}"
        );
    }
}

/// A folder of this test's own in iCloud Drive, removed when dropped.
#[cfg(target_os = "macos")]
struct Probe(PathBuf);

#[cfg(target_os = "macos")]
impl Probe {
    fn new() -> Self {
        let drive = PathBuf::from(std::env::var_os("HOME").unwrap())
            .join("Library/Mobile Documents/com~apple~CloudDocs");
        let mut token = [0u8; 4];
        getrandom::fill(&mut token).unwrap();
        let name = format!(
            "ca-probe-test-{:02x}{:02x}{:02x}{:02x}",
            token[0], token[1], token[2], token[3]
        );
        let folder = drive.join(&name);
        assert!(folder.starts_with(&drive) && name.starts_with("ca-probe-test-"));
        fs::create_dir(&folder).unwrap();
        eprintln!("probe folder {name}");
        Self(folder)
    }

    /// Waits for each file to upload, then makes it a placeholder.
    fn evict(&self, files: &[PathBuf]) {
        assert!(files.iter().all(|file| file.starts_with(&self.0)));
        let asked = |file: &PathBuf| {
            crate::spawn_gate::status(
                Command::new(BRCTL)
                    .arg("evict")
                    .arg(file)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null()),
            )
            .is_ok_and(|status| status.success())
        };
        let evicted =
            |file: &PathBuf| fs::symlink_metadata(file).is_ok_and(|metadata| dataless(&metadata));
        let done = (0..90).any(|_| {
            std::thread::sleep(seconds(2));
            files.iter().filter(|file| asked(file)).count() == files.len()
                && files.iter().all(evicted)
        });
        assert!(done, "the probe files never became placeholders");
    }
}

#[cfg(target_os = "macos")]
impl Drop for Probe {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Makes files placeholders in a folder of iCloud Drive and downloads them again, on a Mac
/// signed in to iCloud Drive. Run with `--ignored`; it waits for the upload before evicting.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs iCloud Drive on this Mac and a network"]
fn placeholders_in_icloud_drive_are_counted_downloaded_and_a_cancel_stops_the_wait() {
    let probe = Probe::new();
    let folder = probe.0.clone();
    fs::create_dir_all(folder.join("deep/er")).unwrap();
    let mut files = Vec::new();
    for index in 0..40 {
        let file = if index % 2 == 0 {
            folder.join(format!("f{index}.txt"))
        } else {
            folder.join(format!("deep/er/f{index}.txt"))
        };
        fs::write(&file, format!("probe {index}\n")).unwrap();
        files.push(file);
    }
    probe.evict(&files);
    let left = placeholders(&folder, COPY_BOUNDS).unwrap();
    assert_eq!(left.paths.len(), files.len());
    assert_eq!(left.largest_bytes, "probe 10\n".len() as u64);
    let measured =
        super::super::folder_size(folder.to_str().unwrap(), Instant::now() + seconds(30)).unwrap();
    assert_eq!(measured.placeholders, files.len() as u64);
    let cancelled = Download::default();
    cancelled.cancel();
    assert_eq!(
        super::download(folder.to_str().unwrap(), &cancelled)
            .unwrap_err()
            .0,
        "home_icloud_not_downloaded"
    );
    let download = Download::default();
    assert_eq!(
        super::download(folder.to_str().unwrap(), &download).unwrap(),
        files.len() as u64
    );
    assert_eq!(
        placeholders(&folder, COPY_BOUNDS).unwrap(),
        Placeholders::default()
    );
    assert_eq!(
        download.progress(),
        Progress {
            total: files.len() as u64,
            done: files.len() as u64
        }
    );
    assert_eq!(fs::read_to_string(&files[3]).unwrap(), "probe 3\n");
}

/// Copies and moves a folder of iCloud Drive holding a placeholder, on a Mac signed in to
/// iCloud Drive: both refuse before reading it, so nothing is fetched, copied or deleted.
/// Run with `--ignored`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs iCloud Drive on this Mac and a network"]
fn a_copy_or_move_meeting_a_placeholder_refuses_and_leaves_the_source_as_it_was() {
    let probe = Probe::new();
    let folder = probe.0.clone();
    fs::create_dir(folder.join("sub")).unwrap();
    fs::write(folder.join("a.txt"), "kept\n").unwrap();
    let only_in_icloud = folder.join("sub/b.txt");
    fs::write(&only_in_icloud, "only in iCloud\n").unwrap();
    probe.evict(std::slice::from_ref(&only_in_icloud));
    let target = tempfile::tempdir().unwrap();
    let copied = super::super::copy_tree(&folder, &target.path().join("copied"));
    let moved = super::super::move_by_copy(&folder, &target.path().join("moved"));
    let still = fs::symlink_metadata(&only_in_icloud).map(|metadata| dataless(&metadata));
    assert_eq!(copied.unwrap_err().0, "home_icloud_not_downloaded");
    assert_eq!(moved.unwrap_err().0, "home_icloud_not_downloaded");
    assert!(still.unwrap(), "the placeholder was fetched");
    assert_eq!(fs::read_to_string(folder.join("a.txt")).unwrap(), "kept\n");
    assert!(fs::symlink_metadata(target.path().join("moved")).is_err());
}

/// Downloads an evicted folder of iCloud Drive that the runner names in
/// `CRAZYAGENT_ICLOUD_SCRATCH` through the real brctl, each ask held `SLOW_ASK` first as the
/// engine's asks took seconds in the owner's run, and checks that the recorded progress never
/// trails the files on disk by more than two polls while later batches still wait to be asked.
/// Run with `--ignored` by its exact name. The folder is kept; its files are local afterwards.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs an evicted folder of iCloud Drive named in CRAZYAGENT_ICLOUD_SCRATCH"]
fn progress_follows_files_landing_from_icloud_within_two_polls() {
    const SLOW_ASK: Duration = Duration::from_secs(3);
    let folder = PathBuf::from(std::env::var_os("CRAZYAGENT_ICLOUD_SCRATCH").unwrap());
    let drive = PathBuf::from(std::env::var_os("HOME").unwrap())
        .join("Library/Mobile Documents/com~apple~CloudDocs");
    let root = folder.canonicalize().unwrap();
    assert!(folder.starts_with(&drive) && folder != drive);
    let total = placeholders(&root, COPY_BOUNDS).unwrap().paths.len() as u64;
    assert!(total > 8 * BATCH_PATHS_MAX as u64, "evict the folder first");
    let slow = |batch: &[PathBuf], stop: &dyn Fn() -> bool| {
        std::thread::sleep(SLOW_ASK);
        request(batch, stop)
    };
    let list = |root: &Path| placeholders(root, COPY_BOUNDS);
    let download = Download::default();
    let mut landed: Vec<(Instant, u64)> = Vec::new();
    let (mut moved_early, mut trailed) = (false, None);
    let outcome = std::thread::scope(|scope| {
        let running = scope.spawn(|| run(&root, &download, &slow, &list, COPY_BOUNDS, POLL_EVERY));
        for _ in 0..30 * 60 * 4 {
            if running.is_finished() || trailed.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
            let left = placeholders(&root, COPY_BOUNDS).unwrap().paths.len() as u64;
            let now = Instant::now();
            let shown = download.progress();
            landed.push((now, total.saturating_sub(left)));
            let floor = landed
                .iter()
                .filter(|(at, _)| now.duration_since(*at) >= 2 * POLL_EVERY)
                .map(|(_, done)| *done)
                .max()
                .unwrap_or(0);
            if shown.done < floor {
                trailed = Some((shown, floor));
            }
            moved_early |= shown.done > 0 && shown.done < total;
        }
        download.cancel();
        running.join().unwrap()
    });
    assert_eq!(
        trailed, None,
        "shown progress, files on disk two polls earlier"
    );
    assert_eq!(outcome.unwrap(), total);
    assert!(moved_early, "progress moved only at the end");
    assert_eq!(
        placeholders(&root, COPY_BOUNDS).unwrap(),
        Placeholders::default()
    );
}
