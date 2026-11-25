use std::time::{Duration, Instant};

/// How long a group its holder saw end stays refused to every later claim: past
/// `GROUP_FRESH` and the keeper's wait, so a session that saw its pane alive just before
/// another path ended the group finds the number refused and only watches it.
const ENDED_REFUSED: Duration = Duration::from_secs(10);

/// The process groups some path is ending now, with no time, and those a holder saw end,
/// with the time it saw that, so a pane's stop and the terminal host's close never both
/// signal one group and no later path claims a number that may lead a
/// new group by then.
static ENDING: std::sync::Mutex<std::collections::BTreeMap<u32, Option<Instant>>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// One path's hold on ending a process group: while it lives every other path leaves the
/// group alone. Dropping it frees the number, or, once its holder saw the group answer
/// ESRCH, keeps it refused for `ENDED_REFUSED`, as it may lead a new group by then.
pub struct Ending {
    group: u32,
    gone: bool,
}

impl Ending {
    /// Takes the hold on a process group. Input: the group id, a pid above 1. Output: the
    /// hold, or none while another path holds that group or a holder saw it end within
    /// `ENDED_REFUSED`.
    pub fn claim(group: u32) -> Option<Self> {
        assert!(group > 1 && i32::try_from(group).is_ok());
        let mut ending = ENDING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ending.retain(|_, ended| ended.is_none_or(|at| at.elapsed() < ENDED_REFUSED));
        if ending.contains_key(&group) {
            return None;
        }
        ending.insert(group, None);
        Some(Self { group, gone: false })
    }

    /// The group this hold names. Output: its id as a pid.
    #[cfg(unix)]
    pub fn pid(&self) -> rustix::process::Pid {
        i32::try_from(self.group)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
            .expect("a held group id is a pid above 1")
    }

    /// Records that the group answered ESRCH, so dropping this hold keeps its number
    /// refused for `ENDED_REFUSED` instead of freeing it.
    #[cfg(unix)]
    pub fn gone(&mut self) {
        self.gone = true;
    }
}

impl Drop for Ending {
    fn drop(&mut self) {
        let mut ending = ENDING
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if self.gone {
            ending.insert(self.group, Some(Instant::now()));
        } else {
            ending.remove(&self.group);
        }
    }
}
