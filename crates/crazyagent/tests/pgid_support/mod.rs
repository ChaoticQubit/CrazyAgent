use std::time::{Duration, Instant};

/// How long a test waits for a new pane's first process to lead its own group.
const LEAD_WAIT: Duration = Duration::from_secs(2);

/// Waits until a pane's first process leads its own process group, as tmux starts every
/// pane. tmux answers with the pane's pid before that process has run setsid, so under load
/// a check made at once still reads the tmux server's group (gate s18152j). Takes the pane's
/// pid. Returns its group; panics with the last answer when it leads none within `LEAD_WAIT`.
pub fn pane_group(pane: u32) -> rustix::process::Pid {
    let leader = rustix::process::Pid::from_raw(i32::try_from(pane).unwrap()).unwrap();
    let deadline = Instant::now() + LEAD_WAIT;
    loop {
        let group = rustix::process::getpgid(Some(leader));
        if group == Ok(leader) {
            return leader;
        }
        assert!(
            Instant::now() < deadline,
            "tmux makes a pane's first process lead its own group within {LEAD_WAIT:?}: {group:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
