#[cfg(unix)]
use super::MessageId;
#[cfg(unix)]
use super::relay::{self, INBOX, INBOX_MAX, Part, REQUEST_WAIT};
#[cfg(unix)]
use crate::team::tool::Mailbox;
#[cfg(unix)]
use std::collections::{BTreeSet, VecDeque};
use std::io::Write;
use std::path::Path;
#[cfg(unix)]
use std::time::Duration;

#[cfg(unix)]
const POLL: Duration = Duration::from_secs(1);
/// How many polls pass between two reads of the team config, in case a teammate's own
/// entry was written after its monitor started.
#[cfg(unix)]
const IDENTIFY_EVERY: u32 = 10;
/// The most message ids remembered as announced; the oldest are forgotten first.
#[cfg(unix)]
const ANNOUNCED_MAX: usize = 1024;

#[cfg(unix)]
const _: () = assert!(ANNOUNCED_MAX >= 4 * INBOX_MAX);

/// Runs the relay's monitor (K2a) for one Claude Code session: once a second it prints
/// `[crazyagent msg <id>] from <name>: <first 200 bytes>` for each new message waiting for
/// this session in its part of the mailbox, which Claude Code hands the session as a
/// notification that wakes it; the message itself arrives through the PostToolUse or
/// Stop hook. A native teammate's monitor asks the engine for its own messages over the
/// lead's caller socket instead (relay over the caller socket, OQ-7 B). It exits when its
/// parent, the harness, is gone or its output closes. Inputs: the mailbox and the output.
/// Output: Ok once the harness is gone, or the plain reason it stopped.
#[cfg(unix)]
pub fn run(mailbox: &Path, mut output: impl Write) -> Result<(), String> {
    let parent = std::os::unix::process::parent_id();
    if parent <= 1 {
        return Ok(());
    }
    let seen = |who: super::hook::Identity| (who.inbox, who.part);
    let (mut tag, mut part) = seen(super::hook::identify(mailbox, None));
    let mut announced: VecDeque<MessageId> = VecDeque::with_capacity(ANNOUNCED_MAX);
    let mut known: BTreeSet<MessageId> = BTreeSet::new();
    let mut polls: u32 = 0;
    while std::os::unix::process::parent_id() == parent {
        if polls % IDENTIFY_EVERY == IDENTIFY_EVERY - 1 {
            (tag, part) = seen(super::hook::identify(mailbox, None));
        }
        polls = polls.wrapping_add(1);
        let waiting = match &tag {
            Some(_) if part == Part::Teammates => peek(mailbox),
            Some(tag) => Mailbox::open(mailbox)
                .and_then(|root| relay::part_folder(root, part))
                .map(|folder| waiting_in(&folder, tag))
                .unwrap_or_default(),
            None => Vec::new(),
        };
        for (id, notice) in waiting {
            if known.contains(&id) {
                continue;
            }
            writeln!(output, "{notice}")
                .and_then(|()| output.flush())
                .map_err(|_| "coord_watch_output_closed".to_owned())?;
            if announced.len() == ANNOUNCED_MAX
                && let Some(oldest) = announced.pop_front()
            {
                known.remove(&oldest);
            }
            announced.push_back(id);
            known.insert(id);
        }
        assert!(announced.len() <= ANNOUNCED_MAX && known.len() == announced.len());
        std::thread::sleep(POLL);
    }
    Ok(())
}

/// The notices of the letters waiting for one tag in a mailbox folder, at most `INBOX_MAX`.
/// Takes the folder and the tag. Returns each letter's id and notice.
#[cfg(unix)]
fn waiting_in(folder: &Mailbox, tag: &str) -> Vec<(MessageId, String)> {
    folder
        .entries(INBOX)
        .into_iter()
        .take(INBOX_MAX)
        .filter_map(|(_, stem)| relay::read_letter(folder, &format!("{stem}{INBOX}")))
        .filter(|letter| letter.to == tag)
        .map(|letter| (letter.id, letter.notice()))
        .collect()
}

/// Asks the engine over the lead's caller socket for the notices of a native teammate's
/// waiting messages (relay over the caller socket, OQ-7 B): one `<id> <notice>` line each,
/// at most `INBOX_MAX`. Takes the mailbox. Returns each id and notice, none on no answer.
#[cfg(unix)]
fn peek(mailbox: &Path) -> Vec<(MessageId, String)> {
    let Some(answer) = crate::team::caller::relay(mailbox, "peek", REQUEST_WAIT) else {
        return Vec::new();
    };
    let Ok(text) = String::from_utf8(answer) else {
        return Vec::new();
    };
    text.lines()
        .take(INBOX_MAX)
        .filter_map(|line| {
            let (id, notice) = line.split_once(' ')?;
            let id = id.parse::<MessageId>().ok().filter(|id| *id > 0)?;
            (!notice.is_empty()).then(|| (id, notice.to_owned()))
        })
        .collect()
}

/// Refuses the monitor where no parent process id exists to watch the harness by.
/// Inputs: the mailbox and the output, unused. Output: `coord_watch_unavailable`.
#[cfg(not(unix))]
pub fn run(_mailbox: &Path, _output: impl Write) -> Result<(), String> {
    Err("coord_watch_unavailable".to_owned())
}
