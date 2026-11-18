use super::{PIPES_BYTES_MAX, Published, encode_pipes, parse_pipes};
use crazyagent_win_sandbox::{pipe, private};
use std::io::{self, Read, Write};
use std::path::Path;
use std::time::Duration;

/// The D7 pipes file's name in its protected root.
const PIPES_FILE: &str = "pipes";
/// The most entries one sweep of a protected root reads; a root holds a handful.
const ROOT_ENTRIES_MAX: usize = 1024;
/// How many times publish tries the rename over `pipes` while a client holds it open.
const RENAME_TRIES: u32 = 50;
/// The pause between two of those tries; all of them together wait about one second.
const RENAME_PAUSE: Duration = Duration::from_millis(20);
/// Windows' ERROR_ACCESS_DENIED.
const ACCESS_DENIED: i32 = 5;
/// Windows' ERROR_SHARING_VIOLATION: another handle's share mode refuses the access.
const SHARING_VIOLATION: i32 = 32;

/// Removes the staged pipes files earlier starts left in a held root, each judged private
/// before it goes, and leaves every other entry (rv-code2 #554 N4). Input: the root. Output:
/// Ok, or the error reading the root, judging a leftover or removing it.
fn sweep(root: &Path) -> io::Result<()> {
    for entry in std::fs::read_dir(root)?.take(ROOT_ENTRIES_MAX) {
        let name = entry?.file_name();
        let Some(name) = name.to_str().filter(|name| staged_name(name)) else {
            continue;
        };
        let leftover = root.join(name);
        drop(private::read_only(&leftover)?);
        std::fs::remove_file(&leftover)?;
    }
    Ok(())
}

/// Renames the staged pipes file over `pipes` (rv-code2 #554 N3). A client reading `pipes`
/// holds it without delete sharing for the moment its read takes, so a rename refused for
/// that is tried again, at most [`RENAME_TRIES`] times. Inputs: the staged file and the
/// target. Output: Ok, or the last rename error.
fn replace(staged: &Path, target: &Path) -> io::Result<()> {
    let mut tries = 1;
    loop {
        match std::fs::rename(staged, target) {
            Err(error) if held_open(&error) && tries < RENAME_TRIES => {
                tries += 1;
                std::thread::sleep(RENAME_PAUSE);
            }
            done => return done,
        }
    }
}

/// Whether a rename failed because another handle holds the target open: Windows' sharing
/// violation, or access denied, which a rename over a file open without delete sharing may
/// return. Input: the error. Output: whether to try again.
fn held_open(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(SHARING_VIOLATION | ACCESS_DENIED)
    )
}

/// Whether a root entry is a staged pipes file: `pipes.`, 16 lowercase hex, `.new`. Input:
/// the entry name. Output: whether publish made it.
fn staged_name(name: &str) -> bool {
    name.strip_prefix(PIPES_FILE)
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(".new"))
        .is_some_and(|hex| {
            hex.len() == 16
                && hex
                    .bytes()
                    .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        })
}

/// Publishes a new start of the endpoint `purpose` in its protected root (DESIGN-0256 D7,
/// leader 07:46): a fresh random part and this engine's pin, staged in a file created private
/// and labelled no-read-up before its first byte (#555 N1), renamed over `pipes` inside the
/// held root, then read back through the private judge. A `pipes` file the judge refuses is
/// left as it is. Inputs: the root and the purpose. Output: what was published, or the error.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the D8 Windows listener is its first caller")
)]
pub(super) fn publish(root: &Path, purpose: &str) -> io::Result<Published> {
    let held = private::dir(root)?;
    let target = root.join(PIPES_FILE);
    match private::read_only(&target) {
        Ok(kept) => drop(kept),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    sweep(root)?;
    let (mut random, mut stage) = ([0; 16], [0; 8]);
    getrandom::fill(&mut random)
        .and_then(|()| getrandom::fill(&mut stage))
        .map_err(|_| io::Error::other("pipe_entropy_unavailable"))?;
    let pin = pipe::own_pin()?;
    let published = Published {
        random,
        pid: pin.pid,
        created: pin.created,
    };
    let stage: String = stage.iter().map(|byte| format!("{byte:02x}")).collect();
    let staged = root.join(format!("{PIPES_FILE}.{stage}.new"));
    let mut file = private::secret_file(&staged)?;
    let written = file
        .write_all(encode_pipes(purpose, &published).as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written.and_then(|()| replace(&staged, &target)) {
        let _ = std::fs::remove_file(&staged);
        return Err(error);
    }
    private::sync_folder(root)?;
    drop(held);
    if read(root, purpose)? != published {
        return Err(io::Error::other(
            "the pipes file changed while the engine published it",
        ));
    }
    Ok(published)
}

/// Reads the endpoint `purpose`'s D7 pipes file in its protected root through the private
/// judge, so only a file private to this user names a pipe or its server (DESIGN-0256 D7).
/// Inputs: the root and the purpose. Output: what the engine published, or why the file is
/// not trusted.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "the D8 Windows listener is its first caller")
)]
pub(super) fn read(root: &Path, purpose: &str) -> io::Result<Published> {
    let file = private::read_only(&root.join(PIPES_FILE))?;
    let mut bytes = Vec::with_capacity(PIPES_BYTES_MAX + 1);
    (&file)
        .take(PIPES_BYTES_MAX as u64 + 1)
        .read_to_end(&mut bytes)?;
    parse_pipes(&bytes, purpose)
        .map_err(|reason| io::Error::new(io::ErrorKind::InvalidData, reason))
}
