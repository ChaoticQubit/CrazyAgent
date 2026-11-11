use super::*;

fn seconds(count: u64) -> Duration {
    Duration::from_secs(count)
}

#[test]
fn a_download_keeps_waiting_while_files_arrive_and_stops_after_ten_quiet_minutes() {
    let start = Instant::now();
    let mut watch = Watch::new(start, start + DOWNLOAD_TIME_MAX);
    assert_eq!(watch.observe(0, start), Verdict::Wait);
    assert_eq!(watch.observe(5, start + seconds(300)), Verdict::Wait);
    assert_eq!(watch.observe(5, start + seconds(899)), Verdict::Wait);
    assert_eq!(watch.observe(6, start + seconds(1199)), Verdict::Wait);
    assert_eq!(watch.observe(6, start + seconds(1798)), Verdict::Wait);
    assert_eq!(
        watch.observe(6, start + seconds(1199) + STALL_MAX),
        Verdict::Stalled
    );
}

#[test]
fn a_download_whose_count_drops_back_is_not_progress() {
    let start = Instant::now();
    let mut watch = Watch::new(start, start + DOWNLOAD_TIME_MAX);
    assert_eq!(watch.observe(10, start), Verdict::Wait);
    assert_eq!(watch.observe(4, start + seconds(60)), Verdict::Wait);
    assert_eq!(watch.observe(9, start + STALL_MAX), Verdict::Stalled);
}

#[test]
fn a_download_still_making_progress_stops_at_its_deadline() {
    let start = Instant::now();
    let deadline = start + seconds(100);
    let mut watch = Watch::new(start, deadline);
    assert_eq!(watch.observe(1, start + seconds(99)), Verdict::Wait);
    assert_eq!(watch.observe(2, deadline), Verdict::TimedOut);
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

#[cfg(unix)]
#[test]
fn a_folder_with_no_placeholders_downloads_nothing_and_never_runs_brctl() {
    let folder = tempfile::tempdir().unwrap();
    fs::create_dir_all(folder.path().join("src/deep")).unwrap();
    fs::write(folder.path().join("src/deep/main.rs"), "fn main() {}\n").unwrap();
    std::os::unix::fs::symlink("/", folder.path().join("root-link")).unwrap();
    assert_eq!(placeholders(folder.path()).unwrap(), Vec::<PathBuf>::new());
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

/// Makes files placeholders in a folder of iCloud Drive and downloads them again, on a Mac
/// signed in to iCloud Drive. Run with `--ignored`; it waits for the upload before evicting.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "needs iCloud Drive on this Mac and a network"]
fn placeholders_in_icloud_drive_are_counted_downloaded_and_a_cancel_stops_the_wait() {
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
    let evict = |file: &Path| {
        crate::spawn_gate::status(
            Command::new(BRCTL)
                .arg("evict")
                .arg(file)
                .stdout(Stdio::null())
                .stderr(Stdio::null()),
        )
        .is_ok_and(|status| status.success())
    };
    let uploaded = (0..60).any(|_| {
        std::thread::sleep(seconds(2));
        files.iter().all(|file| evict(file))
    });
    assert!(uploaded, "the probe files never finished uploading");
    assert_eq!(placeholders(&folder).unwrap().len(), files.len());
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
    assert_eq!(placeholders(&folder).unwrap(), Vec::<PathBuf>::new());
    assert_eq!(
        download.progress(),
        Progress {
            total: files.len() as u64,
            done: files.len() as u64
        }
    );
    assert_eq!(fs::read_to_string(&files[3]).unwrap(), "probe 3\n");
    fs::remove_dir_all(&folder).unwrap();
}
