use super::*;
#[cfg(unix)]
use crate::projects_home::COPY_BOUNDS;

fn seconds(count: u64) -> Duration {
    Duration::from_secs(count)
}

/// What a download of `root` would ask for now, by real flags or this thread's fake ones.
#[cfg(unix)]
fn wanted(root: &Path) -> Vec<PathBuf> {
    let look = |path: &Path| fs::symlink_metadata(path);
    let listing = survey(root, root, 0, COPY_BOUNDS, &look).unwrap();
    listing.wanted.into_iter().map(|entry| entry.path).collect()
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
fn downloads_run_side_by_side_and_each_dropped_claim_cancels_and_lets_go_only_its_own() {
    let downloads = Downloads::default();
    let first = Arc::new(Download::default());
    let second = Arc::new(Download::default());
    let one = downloads.claim_for(Arc::clone(&first)).unwrap();
    let two = downloads.claim_for(Arc::clone(&second)).unwrap();
    assert!(Arc::ptr_eq(&one.download(), &first));
    assert_eq!(downloads.held(), 2);
    drop(one);
    assert!(first.cancelled() && !second.cancelled());
    assert_eq!(downloads.held(), 1);
    drop(two);
    assert!(second.cancelled());
    assert_eq!(downloads.held(), 0);
}

#[test]
#[should_panic(expected = "a download is held once")]
fn a_download_already_held_is_never_held_twice() {
    let downloads = Downloads::default();
    let download = Arc::new(Download::default());
    let _held = downloads.claim_for(Arc::clone(&download)).unwrap();
    let _again = downloads.claim_for(download);
}

#[test]
fn a_skip_list_names_the_first_32_folders_and_counts_the_rest_with_their_files() {
    let folder = |at: usize, files: Option<u64>| Skipped {
        path: format!("p{at}/__pycache__"),
        files,
    };
    let mut gathered = SkippedFolders::default();
    for at in 0..40 {
        gathered.push(folder(at, Some(2)));
    }
    assert_eq!(gathered.listed.len(), SKIPPED_LISTED_MAX);
    let eight = SkippedMore {
        folders: 8,
        files: Some(16),
    };
    assert_eq!(gathered.more, Some(eight));
    assert_eq!(gathered.len(), 40);
    gathered.push(folder(40, None));
    let uncounted = SkippedMore {
        folders: 9,
        files: None,
    };
    assert_eq!(gathered.more, Some(uncounted));
    let (named, more) = gathered.cut(JOB_SKIPPED_LISTED_MAX);
    assert_eq!(named, gathered.listed[..JOB_SKIPPED_LISTED_MAX]);
    let rest = SkippedMore {
        folders: 38,
        files: None,
    };
    assert_eq!(more, Some(rest));
    let few = SkippedFolders {
        listed: vec![folder(0, Some(1))],
        more: None,
    };
    assert_eq!(few.cut(JOB_SKIPPED_LISTED_MAX), (few.listed.clone(), None));
    let mut whole = SkippedFolders::default();
    whole.absorb(gathered.clone());
    whole.absorb(gathered);
    assert_eq!(whole.listed.len(), SKIPPED_LISTED_MAX);
    assert_eq!(whole.len(), 82);
}

#[test]
fn a_shown_path_is_cut_to_its_bound_at_a_character_edge() {
    let root = Path::new("/r");
    let long = format!("{}é/node_modules", "a".repeat(SKIPPED_PATH_MAX - 4));
    let cut = shown(root, &root.join(&long));
    assert!(cut.len() <= SKIPPED_PATH_MAX, "{} bytes", cut.len());
    assert!(cut.ends_with("..."), "{cut}");
    assert!(long.starts_with(cut.trim_end_matches("...")));
    assert_eq!(
        shown(root, &root.join("web/node_modules")),
        "web/node_modules"
    );
}

#[test]
fn the_registry_refuses_a_download_past_its_bound_until_one_ends() {
    let downloads = Downloads::default();
    let mut claims: Vec<Claim<'_>> = (0..DOWNLOADS_MAX)
        .map(|_| downloads.claim_for(Arc::new(Download::default())).unwrap())
        .collect();
    let refused = downloads.claim_for(Arc::new(Download::default()));
    assert_eq!(refused.unwrap_err().0, "icloud_download_busy");
    claims.pop();
    assert!(downloads.claim_for(Arc::new(Download::default())).is_ok());
    assert_eq!(downloads.held(), DOWNLOADS_MAX - 1);
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
    assert!(
        format!("{command:?}").starts_with("env -i "),
        "brctl starts with an empty environment: {command:?}"
    );
}

#[test]
fn the_queue_opens_whole_batches_within_256_per_download_and_512_in_all() {
    let pacer = Pacer::default();
    let (a, b, c) = (pacer.seat(), pacer.seat(), pacer.seat());
    let batch = [1u64; BATCH_PATHS_MAX];
    for seat in [&a, &b, &a, &b] {
        assert_eq!(seat.open(&batch), BATCH_PATHS_MAX);
    }
    assert_eq!(a.open(&batch), 0, "256 open is the most one download holds");
    assert_eq!(c.open(&batch), 0, "512 open is the most of all");
    assert_eq!(pacer.open_now(), (OPEN_FILES_MAX, OPEN_FILES_MAX as u64));
    assert_eq!(
        a.open_now(),
        (JOB_OPEN_FILES_MAX, JOB_OPEN_FILES_MAX as u64)
    );
    assert_eq!(c.open(&[1; 5]), 0);
    a.landed(5, 5);
    assert_eq!(c.open(&[1; 5]), 5, "a short last batch opens once it fits");
    assert_eq!(pacer.open_now(), (OPEN_FILES_MAX, OPEN_FILES_MAX as u64));
}

#[test]
fn the_turn_goes_to_the_download_served_longest_ago() {
    let pacer = Pacer::default();
    let (a, b, c) = (pacer.seat(), pacer.seat(), pacer.seat());
    let batch = [1u64; BATCH_PATHS_MAX];
    for seat in [&a, &b, &a, &b] {
        assert_eq!(seat.open(&batch), BATCH_PATHS_MAX);
    }
    assert_eq!(c.open(&batch), 0);
    a.landed(BATCH_PATHS_MAX, BATCH_PATHS_MAX as u64);
    assert_eq!(a.open(&batch), 0, "c waited longest, so the room is c's");
    assert_eq!(c.open(&batch), BATCH_PATHS_MAX);
    b.landed(BATCH_PATHS_MAX, BATCH_PATHS_MAX as u64);
    assert_eq!(b.open(&batch), 0, "a was served before b");
    assert_eq!(a.open(&batch), BATCH_PATHS_MAX);
}

#[test]
fn two_downloads_alternate_even_when_one_lands_faster() {
    let pacer = Pacer::default();
    let (x, y) = (pacer.seat(), pacer.seat());
    let half = [OPEN_BYTES_MAX / 2 / BATCH_PATHS_MAX as u64; BATCH_PATHS_MAX];
    let bytes: u64 = half.iter().sum();
    assert_eq!(x.open(&half), BATCH_PATHS_MAX);
    assert_eq!(y.open(&half), BATCH_PATHS_MAX);
    assert_eq!(pacer.open_now(), (2 * BATCH_PATHS_MAX, OPEN_BYTES_MAX));
    assert_eq!(
        (x.open(&half), y.open(&half)),
        (0, 0),
        "the byte cap is full"
    );
    x.landed(BATCH_PATHS_MAX, bytes);
    assert_eq!(x.open(&half), BATCH_PATHS_MAX, "x was served first");
    x.landed(BATCH_PATHS_MAX, bytes);
    assert_eq!(x.open(&half), 0, "y waited since before x's last turn");
    assert_eq!(y.open(&half), BATCH_PATHS_MAX);
    assert_eq!(y.open_now(), (2 * BATCH_PATHS_MAX, 2 * bytes));
    y.landed(BATCH_PATHS_MAX, bytes);
    assert_eq!(y.open(&half), 0, "x waited since before y's last turn");
    assert_eq!(x.open(&half), BATCH_PATHS_MAX);
}

#[test]
fn a_file_larger_than_the_byte_cap_opens_alone_and_nothing_opens_before_it() {
    let pacer = Pacer::default();
    let (a, b) = (pacer.seat(), pacer.seat());
    let big = OPEN_BYTES_MAX + 1;
    assert_eq!(a.open(&[1024; 10]), 10);
    assert_eq!(b.open(&[big]), 0, "it waits until nothing is open");
    assert_eq!(a.open(&[1024; 10]), 0, "nothing else opens before it");
    a.landed(10, 10 * 1024);
    assert_eq!(a.open(&[1024]), 0);
    assert_eq!(b.open(&[big]), 1);
    assert_eq!(pacer.open_now(), (1, big));
    assert_eq!(a.open(&[1024]), 0, "nothing opens beside it");
    b.landed(1, big);
    assert_eq!(a.open(&[1024]), 1);
}

#[test]
fn the_byte_cap_opens_only_the_files_that_fit() {
    let mib = 1024 * 1024;
    let pacer = Pacer::default();
    let (a, b) = (pacer.seat(), pacer.seat());
    assert_eq!(a.open(&[600 * mib, 600 * mib]), 1);
    assert_eq!(b.open(&[600 * mib]), 0);
    assert_eq!(b.open(&[400 * mib, 1]), 2);
    assert_eq!(pacer.open_now(), (3, 1000 * mib + 1));
}

#[test]
fn a_download_that_ends_gives_back_its_open_files_at_once() {
    let pacer = Pacer::default();
    let (a, b, c) = (pacer.seat(), pacer.seat(), pacer.seat());
    let batch = [1u64; BATCH_PATHS_MAX];
    for seat in [&a, &b, &a, &b] {
        assert_eq!(seat.open(&batch), BATCH_PATHS_MAX);
    }
    assert_eq!(c.open(&batch), 0);
    drop(a);
    assert_eq!(
        pacer.open_now(),
        (JOB_OPEN_FILES_MAX, JOB_OPEN_FILES_MAX as u64)
    );
    assert_eq!(c.open(&batch), BATCH_PATHS_MAX);
    drop((b, c));
    assert_eq!(pacer.open_now(), (0, 0));
}

#[test]
fn a_first_landing_112_seconds_after_the_ask_and_a_wait_for_the_turn_never_read_as_stalled() {
    let start = Instant::now();
    let mut watch = Watch::new(start, start + DOWNLOAD_TIME_MAX);
    assert_eq!(watch.observe(0, 0, start + seconds(112)), Verdict::Wait);
    assert_eq!(watch.observe(1, 0, start + seconds(113)), Verdict::Wait);
    let waited = start + seconds(113) + seconds(20 * 60);
    assert_eq!(watch.rest(waited), Verdict::Wait);
    assert_eq!(watch.observe(1, 0, waited + seconds(9 * 60)), Verdict::Wait);
    assert_eq!(watch.observe(1, 0, waited + STALL_MAX), Verdict::Stalled);
    assert_eq!(watch.rest(start + DOWNLOAD_TIME_MAX), Verdict::TimedOut);
}

/// Marks an entry as only in iCloud for a thread that asked for [`fake`] placeholders.
#[cfg(unix)]
fn evict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(fake::MODE)).unwrap();
}

/// Lands an entry [`evict`] marked: a plain mode for a file, a folder's for a folder.
#[cfg(unix)]
fn land(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mode = if path.is_dir() { 0o755 } else { 0o644 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Stands in for iCloud on any Unix: a folder of this row's own holding `count` files of
/// `bytes` bytes, each marked as only in iCloud, that remembers what was asked for and lands
/// the asked entries in order, as each ask ends or a few at a time.
#[cfg(unix)]
struct Cloud {
    folder: tempfile::TempDir,
    files: Vec<PathBuf>,
    waiting: Mutex<VecDeque<PathBuf>>,
    asked: Mutex<Vec<PathBuf>>,
    asks: std::sync::atomic::AtomicUsize,
    landed: std::sync::atomic::AtomicUsize,
}

#[cfg(unix)]
impl Cloud {
    fn new(count: usize, bytes: usize) -> Self {
        let folder = tempfile::tempdir().unwrap();
        let files: Vec<PathBuf> = (0..count)
            .map(|n| folder.path().join(format!("f{n:05}")))
            .collect();
        for file in &files {
            fs::write(file, vec![b'x'; bytes]).unwrap();
            evict(file);
        }
        Self {
            folder,
            files,
            waiting: Mutex::default(),
            asked: Mutex::default(),
            asks: Default::default(),
            landed: Default::default(),
        }
    }

    fn root(&self) -> &Path {
        self.folder.path()
    }

    fn ask(&self, batch: &[PathBuf]) -> Result<(), EngineError> {
        assert!(!batch.is_empty() && batch.len() <= BATCH_PATHS_MAX);
        self.asks.fetch_add(1, Ordering::SeqCst);
        self.asked.lock().unwrap().extend_from_slice(batch);
        self.waiting.lock().unwrap().extend(batch.iter().cloned());
        Ok(())
    }

    fn was_asked(&self, path: &Path) -> bool {
        self.asked.lock().unwrap().iter().any(|asked| asked == path)
    }

    /// Lands at most `most` of the entries asked for and not landed yet. Output: how many.
    fn land_some(&self, most: usize) -> usize {
        let mut waiting = self.waiting.lock().unwrap();
        let count = most.min(waiting.len());
        for path in waiting.drain(..count) {
            land(&path);
        }
        self.landed.fetch_add(count, Ordering::SeqCst);
        count
    }

    fn waiting(&self) -> usize {
        self.waiting.lock().unwrap().len()
    }
}

/// Lands `per_tick` asked entries of each cloud every 2 ms until `done` holds.
#[cfg(unix)]
fn lander(clouds: &[&Cloud], per_tick: usize, done: &AtomicBool) {
    while !done.load(Ordering::SeqCst) {
        for cloud in clouds {
            cloud.land_some(per_tick);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[cfg(unix)]
const FAST: Duration = Duration::from_millis(5);

#[cfg(unix)]
#[test]
fn a_download_reads_only_its_open_entries_between_its_two_listings_and_counts_bytes() {
    fake::placeholders();
    let cloud = Cloud::new(700, 3);
    let pacer = Pacer::default();
    let download = Download::default();
    let looks = std::sync::atomic::AtomicUsize::new(0);
    let (most_open, least_open) = (
        std::sync::atomic::AtomicUsize::new(0),
        std::sync::atomic::AtomicUsize::new(usize::MAX),
    );
    let look = |path: &Path| {
        looks.fetch_add(1, Ordering::SeqCst);
        fs::symlink_metadata(path)
    };
    let ask = |batch: &[PathBuf], _: &dyn Fn() -> bool| {
        let open = pacer.open_now().0;
        most_open.fetch_max(open, Ordering::SeqCst);
        least_open.fetch_min(open, Ordering::SeqCst);
        let asked = cloud.ask(batch);
        cloud.land_some(BATCH_PATHS_MAX);
        asked
    };
    let poll = Duration::from_millis(20);
    let files = run(
        cloud.root(),
        &download,
        &pacer,
        &ask,
        &look,
        COPY_BOUNDS,
        poll,
    );
    assert_eq!(files.unwrap(), 700);
    let (most, least) = (
        most_open.load(Ordering::SeqCst),
        least_open.load(Ordering::SeqCst),
    );
    assert!(most <= JOB_OPEN_FILES_MAX, "{most} open at once");
    assert!(
        least >= 700 % BATCH_PATHS_MAX,
        "{least} open when a batch was asked"
    );
    assert_eq!(
        download.progress(),
        Progress {
            total: 700,
            done: 700,
            bytes_total: 2100,
            bytes_done: 2100,
        }
    );
    assert_eq!(pacer.open_now(), (0, 0));
    let looks = looks.load(Ordering::SeqCst);
    assert!(
        looks <= 2 * 701 + 4 * 700,
        "{looks} reads: the start and check listings and each asked file a few times"
    );
}

#[cfg(unix)]
#[test]
fn three_downloads_share_the_queue_and_never_open_more_than_its_caps() {
    let clouds = [Cloud::new(600, 1), Cloud::new(600, 1), Cloud::new(600, 1)];
    let pacer = Pacer::default();
    let most = std::sync::atomic::AtomicUsize::new(0);
    let done = AtomicBool::new(false);
    let outcomes: Vec<Result<u64, EngineError>> = std::thread::scope(|scope| {
        scope.spawn(|| lander(&[&clouds[0], &clouds[1], &clouds[2]], 3, &done));
        let running: Vec<_> = clouds
            .iter()
            .map(|cloud| {
                let (pacer, most) = (&pacer, &most);
                scope.spawn(move || {
                    fake::placeholders();
                    let ask = |batch: &[PathBuf], _: &dyn Fn() -> bool| {
                        let asked = cloud.ask(batch);
                        assert!(cloud.waiting() <= JOB_OPEN_FILES_MAX);
                        most.fetch_max(pacer.open_now().0, Ordering::SeqCst);
                        asked
                    };
                    let look = |path: &Path| fs::symlink_metadata(path);
                    let download = Download::default();
                    run(
                        cloud.root(),
                        &download,
                        pacer,
                        &ask,
                        &look,
                        COPY_BOUNDS,
                        FAST,
                    )
                })
            })
            .collect();
        let outcomes = running.into_iter().map(|job| job.join().unwrap()).collect();
        done.store(true, Ordering::SeqCst);
        outcomes
    });
    for outcome in outcomes {
        assert_eq!(outcome.unwrap(), 600);
    }
    let most = most.load(Ordering::SeqCst);
    assert!(most <= OPEN_FILES_MAX, "{most} open at once");
    assert!(
        most > JOB_OPEN_FILES_MAX,
        "the downloads ran side by side: {most}"
    );
    assert_eq!(pacer.open_now(), (0, 0));
}

#[cfg(unix)]
#[test]
fn one_cancelled_download_leaves_the_other_running_and_gives_back_its_files() {
    let (kept, cancelled) = (Cloud::new(600, 1), Cloud::new(600, 1));
    let pacer = Pacer::default();
    let done = AtomicBool::new(false);
    let (kept_outcome, cancelled_outcome, open_after_cancel) = std::thread::scope(|scope| {
        scope.spawn(|| lander(&[&kept, &cancelled], 2, &done));
        let stopping = scope.spawn(|| {
            fake::placeholders();
            let download = Download::default();
            let ask = |batch: &[PathBuf], _: &dyn Fn() -> bool| {
                if cancelled.asks.load(Ordering::SeqCst) == 1 {
                    download.cancel();
                }
                cancelled.ask(batch)
            };
            let look = |path: &Path| fs::symlink_metadata(path);
            let outcome = run(
                cancelled.root(),
                &download,
                &pacer,
                &ask,
                &look,
                COPY_BOUNDS,
                FAST,
            );
            (outcome, pacer.open_now().0)
        });
        let running = scope.spawn(|| {
            fake::placeholders();
            let download = Download::default();
            let ask = |batch: &[PathBuf], _: &dyn Fn() -> bool| kept.ask(batch);
            let look = |path: &Path| fs::symlink_metadata(path);
            run(
                kept.root(),
                &download,
                &pacer,
                &ask,
                &look,
                COPY_BOUNDS,
                FAST,
            )
        });
        let (cancelled_outcome, open_after_cancel) = stopping.join().unwrap();
        let kept_outcome = running.join().unwrap();
        done.store(true, Ordering::SeqCst);
        (kept_outcome, cancelled_outcome, open_after_cancel)
    });
    assert_eq!(cancelled_outcome.unwrap_err().0, NOT_DOWNLOADED);
    assert!(cancelled.landed.load(Ordering::SeqCst) < 600);
    assert!(
        open_after_cancel <= JOB_OPEN_FILES_MAX,
        "{open_after_cancel}"
    );
    assert_eq!(kept_outcome.unwrap(), 600);
    assert_eq!(pacer.open_now(), (0, 0));
}

#[cfg(unix)]
#[test]
fn a_folder_that_lands_is_listed_alone_and_its_files_asked_within_the_bounds() {
    fake::placeholders();
    let cloud = Cloud::new(1, 1);
    let sub = cloud.root().join("sub");
    fs::create_dir_all(sub.join("deep")).unwrap();
    for name in ["a", "b", "deep/c"] {
        fs::write(sub.join(name), "x").unwrap();
        evict(&sub.join(name));
    }
    evict(&sub);
    let ask = |batch: &[PathBuf], _: &dyn Fn() -> bool| {
        let asked = cloud.ask(batch);
        cloud.land_some(BATCH_PATHS_MAX);
        asked
    };
    let look = |path: &Path| fs::symlink_metadata(path);
    let shallow = CopyBounds {
        depth: 1,
        ..COPY_BOUNDS
    };
    let download = Download::default();
    let too_deep = run(
        cloud.root(),
        &download,
        &Pacer::default(),
        &ask,
        &look,
        shallow,
        FAST,
    );
    assert_eq!(too_deep.unwrap_err().0, "home_copy_too_large");
    assert!(cloud.was_asked(&sub), "the folder itself was asked for");
    assert!(
        !cloud.was_asked(&sub.join("a")),
        "past the bounds nothing below it is"
    );
    evict(&sub);
    evict(&cloud.files[0]);
    let download = Download::default();
    let files = run(
        cloud.root(),
        &download,
        &Pacer::default(),
        &ask,
        &look,
        COPY_BOUNDS,
        FAST,
    );
    assert_eq!(files.unwrap(), 5, "f00000, sub, a, b and deep/c");
    assert!(cloud.was_asked(&sub.join("deep/c")));
    assert_eq!(download.progress().total, 5);
    assert_eq!(download.progress().done, 5);
}

#[cfg(unix)]
#[test]
fn the_check_listing_asks_again_for_a_file_macos_evicted() {
    fake::placeholders();
    let cloud = Cloud::new(50, 2);
    let first = cloud.files[0].clone();
    let evicted = AtomicBool::new(false);
    let look = |path: &Path| {
        let all = cloud.landed.load(Ordering::SeqCst) == cloud.files.len();
        if path == cloud.root() && all && !evicted.swap(true, Ordering::SeqCst) {
            evict(&first);
        }
        fs::symlink_metadata(path)
    };
    let ask = |batch: &[PathBuf], _: &dyn Fn() -> bool| {
        let asked = cloud.ask(batch);
        cloud.land_some(BATCH_PATHS_MAX);
        asked
    };
    let download = Download::default();
    let files = run(
        cloud.root(),
        &download,
        &Pacer::default(),
        &ask,
        &look,
        COPY_BOUNDS,
        FAST,
    );
    assert_eq!(files.unwrap(), 50);
    let asked = cloud.asked.lock().unwrap().clone();
    assert_eq!(asked.iter().filter(|path| **path == first).count(), 2);
    assert!(!dataless(&fs::symlink_metadata(&first).unwrap()));
    assert_eq!(
        download.progress(),
        Progress {
            total: 50,
            done: 50,
            bytes_total: 100,
            bytes_done: 100,
        }
    );
}

#[cfg(unix)]
#[test]
fn files_that_land_while_slow_asks_still_go_are_counted_before_the_last_ask_ends() {
    fake::placeholders();
    let batches = 6;
    let cloud = Cloud::new(BATCH_PATHS_MAX * batches, 1);
    let download = Download::default();
    let seen = AtomicU64::new(0);
    let ask = |batch: &[PathBuf], _stop: &dyn Fn() -> bool| {
        seen.fetch_max(download.progress().done, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(80));
        let asked = cloud.ask(batch);
        cloud.land_some(BATCH_PATHS_MAX);
        asked
    };
    let look = |path: &Path| fs::symlink_metadata(path);
    let poll = Duration::from_millis(10);
    let files = run(
        cloud.root(),
        &download,
        &Pacer::default(),
        &ask,
        &look,
        COPY_BOUNDS,
        poll,
    );
    let total = (BATCH_PATHS_MAX * batches) as u64;
    assert_eq!(files.unwrap(), total);
    assert_eq!(download.progress().done, total);
    assert_eq!(cloud.asks.load(Ordering::SeqCst), batches);
    let counted = seen.load(Ordering::SeqCst);
    assert!(
        counted >= BATCH_PATHS_MAX as u64,
        "progress read {counted} while later asks still waited"
    );
    assert!(counted < total);
}

#[cfg(unix)]
#[test]
fn a_failed_ask_ends_the_download_with_its_code_and_asks_nothing_more() {
    fake::placeholders();
    let cloud = Cloud::new(BATCH_PATHS_MAX * 4, 1);
    let download = Download::default();
    let ask = |batch: &[PathBuf], _stop: &dyn Fn() -> bool| {
        if cloud.asks.load(Ordering::SeqCst) == 1 {
            cloud.asks.fetch_add(1, Ordering::SeqCst);
            return Err(refused(NOT_DOWNLOADED));
        }
        let asked = cloud.ask(batch);
        cloud.land_some(BATCH_PATHS_MAX);
        asked
    };
    let look = |path: &Path| fs::symlink_metadata(path);
    let started = Instant::now();
    let failed = run(
        cloud.root(),
        &download,
        &Pacer::default(),
        &ask,
        &look,
        COPY_BOUNDS,
        FAST,
    );
    assert_eq!(failed.unwrap_err().0, NOT_DOWNLOADED);
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(cloud.asks.load(Ordering::SeqCst), 2);
    assert!(download.progress().done < download.progress().total);
}

#[cfg(unix)]
#[test]
fn a_cancel_during_slow_asks_stops_the_ask_and_the_download_within_one_check() {
    fake::placeholders();
    let cloud = Cloud::new(BATCH_PATHS_MAX * 4, 1);
    let download = Download::default();
    let ask = |_: &[PathBuf], stop: &dyn Fn() -> bool| {
        cloud.asks.fetch_add(1, Ordering::SeqCst);
        download.cancel();
        let until = Instant::now() + Duration::from_secs(30);
        while Instant::now() < until && !stop() {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(stop(), "the ask sees the cancel");
        Err(stopped("stopped"))
    };
    let look = |path: &Path| fs::symlink_metadata(path);
    let started = Instant::now();
    let pacer = Pacer::default();
    let cancelled = run(
        cloud.root(),
        &download,
        &pacer,
        &ask,
        &look,
        COPY_BOUNDS,
        FAST,
    );
    assert_eq!(cancelled.unwrap_err().0, NOT_DOWNLOADED);
    assert!(started.elapsed() < CANCEL_EVERY * 5);
    assert_eq!(cloud.asks.load(Ordering::SeqCst), 1);
    assert_eq!(pacer.open_now(), (0, 0));
}

/// A project folder whose rebuildable folders sit in every state the skip rule meets, with
/// `dist/` named by its `.gitignore` and `build` not.
#[cfg(unix)]
fn project() -> tempfile::TempDir {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    for dir in [
        "src",
        "node_modules/left-pad",
        ".venv/lib",
        "web/__pycache__",
        "dist",
        "build",
        ".git/objects",
    ] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    for (file, text) in [
        (".gitignore", "# build output\ndist/\n*.log\n"),
        ("src/main.rs", "fn main() {}\n"),
        ("src/data.bin", "only in iCloud"),
        ("node_modules/left-pad/index.js", "module.exports = 1;\n"),
        ("node_modules/left-pad/package.json", "{}\n"),
        (".venv/lib/site.py", "x = 1\n"),
        ("web/__pycache__/app.pyc", "pyc"),
        ("dist/app.js", "bundle"),
        ("build/out.o", "object"),
        (".git/objects/ab", "blob"),
    ] {
        fs::write(root.join(file), text).unwrap();
    }
    for placeholder in [
        "src/data.bin",
        "node_modules/left-pad/index.js",
        "web/__pycache__",
        "dist/app.js",
        "build/out.o",
        ".git/objects/ab",
    ] {
        evict(&root.join(placeholder));
    }
    folder
}

#[cfg(unix)]
fn listed(root: &Path, bounds: CopyBounds) -> (Vec<String>, Vec<Skipped>) {
    let look = |path: &Path| fs::symlink_metadata(path);
    let listing = survey(root, root, 0, bounds, &look).unwrap();
    let mut wanted: Vec<String> = listing
        .wanted
        .iter()
        .map(|entry| shown(root, &entry.path))
        .collect();
    assert_eq!(listing.skipped.more, None);
    let mut skipped = listing.skipped.listed;
    wanted.sort();
    skipped.sort_by(|a, b| a.path.cmp(&b.path));
    (wanted, skipped)
}

#[cfg(unix)]
#[test]
fn rebuildable_folders_still_needing_download_are_skipped_with_their_file_counts() {
    fake::placeholders();
    let folder = project();
    let root = folder.path();
    let skipping = CopyBounds {
        skip_rebuildable: true,
        ..COPY_BOUNDS
    };
    let (wanted, skipped) = listed(root, skipping);
    assert_eq!(wanted, [".git/objects/ab", "build/out.o", "src/data.bin"]);
    assert_eq!(
        skipped,
        [
            Skipped {
                path: "dist".into(),
                files: Some(1),
            },
            Skipped {
                path: "node_modules".into(),
                files: Some(2),
            },
            Skipped {
                path: "web/__pycache__".into(),
                files: None,
            },
        ]
    );
    let look = |path: &Path| fs::symlink_metadata(path);
    let left = |name: &str| {
        let path = root.join(name);
        let metadata = fs::symlink_metadata(&path).unwrap();
        left_behind(root, &path, &metadata, skipping, &look)
    };
    assert!(left("node_modules") && left("dist") && left("web/__pycache__"));
    assert!(
        !left(".venv"),
        "a rebuildable folder fully on this Mac moves as it is"
    );
    assert!(!left("build") && !left(".git") && !left("src"));
}

#[cfg(unix)]
#[test]
fn download_everything_lists_the_rebuildable_folders_too() {
    fake::placeholders();
    let folder = project();
    let root = folder.path();
    const { assert!(!COPY_BOUNDS.skip_rebuildable) }
    let (wanted, skipped) = listed(root, COPY_BOUNDS);
    assert_eq!(
        wanted,
        [
            ".git/objects/ab",
            "build/out.o",
            "dist/app.js",
            "node_modules/left-pad/index.js",
            "src/data.bin",
            "web/__pycache__",
        ]
    );
    assert!(skipped.is_empty());
    let look = |path: &Path| fs::symlink_metadata(path);
    let modules = root.join("node_modules");
    let metadata = fs::symlink_metadata(&modules).unwrap();
    assert!(!left_behind(root, &modules, &metadata, COPY_BOUNDS, &look));
}

#[cfg(unix)]
#[test]
fn a_download_keeps_the_folders_it_left_so_the_job_can_say_where_they_stayed() {
    fake::placeholders();
    let folder = project();
    let root = folder.path();
    let skipping = CopyBounds {
        skip_rebuildable: true,
        ..COPY_BOUNDS
    };
    let look = |path: &Path| fs::symlink_metadata(path);
    let ask = |batch: &[PathBuf], _: &dyn Fn() -> bool| {
        batch.iter().for_each(|path| land(path));
        Ok::<(), EngineError>(())
    };
    assert!(Download::default().skipped().is_empty());
    let download = Download::default();
    let asked = run(
        root,
        &download,
        &Pacer::default(),
        &ask,
        &look,
        skipping,
        FAST,
    );
    assert_eq!(asked.unwrap(), 3);
    let mut left = download.skipped().listed;
    left.sort_by(|a, b| a.path.cmp(&b.path));
    let paths: Vec<&str> = left.iter().map(|folder| folder.path.as_str()).collect();
    assert_eq!(paths, ["dist", "node_modules", "web/__pycache__"]);

    let only = tempfile::tempdir().unwrap();
    let module = only.path().join("node_modules/a/index.js");
    fs::create_dir_all(module.parent().unwrap()).unwrap();
    fs::write(&module, "1").unwrap();
    evict(&module);
    let nothing = Download::default();
    let asked = run(
        only.path(),
        &nothing,
        &Pacer::default(),
        &ask,
        &look,
        skipping,
        FAST,
    );
    assert_eq!(
        asked.unwrap(),
        0,
        "only a left folder needs iCloud, so nothing is asked"
    );
    assert_eq!(
        nothing.skipped().listed,
        [Skipped {
            path: "node_modules".into(),
            files: Some(1),
        }]
    );
}

#[test]
fn a_gitignore_line_names_a_folder_only_plainly() {
    for line in ["dist", "dist/", "/dist", "**/dist", "**/dist/"] {
        assert!(names(line, "dist", "dist"), "{line}");
    }
    for line in ["web/dist", "/web/dist", "web/dist/", "dist", "**/dist"] {
        assert!(names(line, "dist", "web/dist"), "{line}");
    }
    for line in [
        "/dist",
        "dist*",
        "*",
        "d?st",
        "[d]ist",
        "**/web/dist",
        "build",
        "/",
        "",
    ] {
        assert!(!names(line, "dist", "web/dist"), "{line}");
    }
}

#[cfg(unix)]
#[test]
fn dist_and_build_are_rebuildable_only_where_a_trusted_gitignore_names_them_and_none_negates() {
    fake::placeholders();
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path();
    for dir in ["dist", "build", "web/build", "web/dist"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    let look = |path: &Path| fs::symlink_metadata(path);
    let rebuilt = |name: &str| rebuildable(root, &root.join(name), &look);
    assert!(
        !rebuilt("dist") && !rebuilt("build"),
        "no .gitignore names them"
    );
    assert!(rebuilt("node_modules") && rebuilt(".venv") && rebuilt("__pycache__"));
    assert!(!rebuilt(".git") && !rebuilt("src"));
    fs::write(root.join(".gitignore"), "/build\n").unwrap();
    fs::write(root.join("web/.gitignore"), "build/\n").unwrap();
    assert!(rebuilt("build") && rebuilt("web/build"));
    assert!(!rebuilt("dist") && !rebuilt("web/dist"));
    fs::write(root.join("web/.gitignore"), "build/\n!build/keep.txt\n").unwrap();
    assert!(!rebuilt("web/build"), "any negation leaves it to move");
    assert!(
        rebuilt("build"),
        "a negation below does not reach the root's build"
    );
    fs::write(root.join("web/.gitignore"), "build/\n").unwrap();
    evict(&root.join("web/.gitignore"));
    assert!(
        !rebuilt("web/build"),
        "a .gitignore still in iCloud is not read"
    );
    fs::remove_file(root.join("web/.gitignore")).unwrap();
    fs::write(root.join("elsewhere"), "build/\n").unwrap();
    std::os::unix::fs::symlink(root.join("elsewhere"), root.join("web/.gitignore")).unwrap();
    assert!(
        !rebuilt("web/build"),
        "a .gitignore that is a link is not followed"
    );
    fs::remove_file(root.join("web/.gitignore")).unwrap();
    fs::write(
        root.join("web/.gitignore"),
        vec![b'#'; GITIGNORE_BYTES_MAX as usize + 1],
    )
    .unwrap();
    assert!(!rebuilt("web/build"), "an oversized .gitignore is not read");
}

#[cfg(unix)]
#[test]
fn a_folder_with_no_placeholders_downloads_nothing_and_never_runs_brctl() {
    let folder = tempfile::tempdir().unwrap();
    fs::create_dir_all(folder.path().join("src/deep")).unwrap();
    fs::write(folder.path().join("src/deep/main.rs"), "fn main() {}\n").unwrap();
    std::os::unix::fs::symlink("/", folder.path().join("root-link")).unwrap();
    assert!(wanted(folder.path()).is_empty());
    let download = Download::default();
    let path = folder.path().to_str().unwrap();
    assert_eq!(super::download(path, &download, COPY_BOUNDS).unwrap(), 0);
    assert_eq!(download.progress(), Progress::default());
    for (path, code) in [
        ("relative", "icloud_path_invalid"),
        ("/no/such/folder/here", "icloud_folder_missing"),
        ("/", "icloud_folder_unsafe"),
    ] {
        assert_eq!(
            super::download(path, &download, COPY_BOUNDS).unwrap_err().0,
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
    assert_eq!(wanted(&folder).len(), files.len());
    let measured =
        super::super::folder_size(folder.to_str().unwrap(), Instant::now() + seconds(30)).unwrap();
    assert_eq!(measured.placeholders, files.len() as u64);
    let cancelled = Download::default();
    cancelled.cancel();
    assert_eq!(
        super::download(folder.to_str().unwrap(), &cancelled, COPY_BOUNDS)
            .unwrap_err()
            .0,
        "home_icloud_not_downloaded"
    );
    let download = Download::default();
    assert_eq!(
        super::download(folder.to_str().unwrap(), &download, COPY_BOUNDS).unwrap(),
        files.len() as u64
    );
    assert!(wanted(&folder).is_empty());
    let progress = download.progress();
    assert_eq!(
        (progress.total, progress.done),
        (files.len() as u64, files.len() as u64)
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
    let total = wanted(&root).len() as u64;
    assert!(total > 8 * BATCH_PATHS_MAX as u64, "evict the folder first");
    let slow = |batch: &[PathBuf], stop: &dyn Fn() -> bool| {
        std::thread::sleep(SLOW_ASK);
        request(batch, stop)
    };
    let look = |path: &Path| fs::symlink_metadata(path);
    let pacer = Pacer::default();
    let download = Download::default();
    let mut landed: Vec<(Instant, u64)> = Vec::new();
    let (mut moved_early, mut trailed) = (false, None);
    let outcome = std::thread::scope(|scope| {
        let running = scope.spawn(|| {
            run(
                &root,
                &download,
                &pacer,
                &slow,
                &look,
                COPY_BOUNDS,
                POLL_EVERY,
            )
        });
        for _ in 0..30 * 60 * 4 {
            if running.is_finished() || trailed.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(250));
            let left = wanted(&root).len() as u64;
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
    assert!(wanted(&root).is_empty());
}
