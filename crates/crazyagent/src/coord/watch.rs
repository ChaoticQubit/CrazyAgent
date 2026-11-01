#[cfg(unix)]
use super::MessageId;
#[cfg(unix)]
use super::relay::{self, INBOX, INBOX_MAX};
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
/// `[crazyagent msg <id>] from <name>: <first 200 bytes>` for each new message waiting in
/// the mailbox for this session, which Claude Code hands the session as a notification
/// that wakes it; the message itself arrives through the PostToolUse or Stop hook. It
/// exits when its parent, the harness, is gone or its output closes. Inputs: the mailbox
/// and the output. Output: Ok once the harness is gone, or the plain reason it stopped.
#[cfg(unix)]
pub fn run(mailbox: &Path, mut output: impl Write) -> Result<(), String> {
    let parent = std::os::unix::process::parent_id();
    if parent <= 1 {
        return Ok(());
    }
    let mut tag = super::hook::identify(None, None).inbox;
    let mut announced: VecDeque<MessageId> = VecDeque::with_capacity(ANNOUNCED_MAX);
    let mut known: BTreeSet<MessageId> = BTreeSet::new();
    let mut polls: u32 = 0;
    while std::os::unix::process::parent_id() == parent {
        if polls % IDENTIFY_EVERY == IDENTIFY_EVERY - 1 {
            tag = super::hook::identify(None, None).inbox;
        }
        polls = polls.wrapping_add(1);
        if let (Some(tag), Ok(folder)) = (&tag, Mailbox::open(mailbox)) {
            for (_, stem) in folder.entries(INBOX).into_iter().take(INBOX_MAX) {
                let Some(letter) = relay::read_letter(&folder, &format!("{stem}{INBOX}")) else {
                    continue;
                };
                if &letter.to != tag || known.contains(&letter.id) {
                    continue;
                }
                writeln!(output, "{}", letter.notice())
                    .and_then(|()| output.flush())
                    .map_err(|_| "coord_watch_output_closed".to_owned())?;
                if announced.len() == ANNOUNCED_MAX
                    && let Some(oldest) = announced.pop_front()
                {
                    known.remove(&oldest);
                }
                announced.push_back(letter.id);
                known.insert(letter.id);
            }
        }
        assert!(announced.len() <= ANNOUNCED_MAX && known.len() == announced.len());
        std::thread::sleep(POLL);
    }
    Ok(())
}

/// Refuses the monitor where no parent process id exists to watch the harness by.
/// Inputs: the mailbox and the output, unused. Output: `coord_watch_unavailable`.
#[cfg(not(unix))]
pub fn run(_mailbox: &Path, _output: impl Write) -> Result<(), String> {
    Err("coord_watch_unavailable".to_owned())
}
