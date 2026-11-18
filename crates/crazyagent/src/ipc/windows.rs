use super::{PIPES_BYTES_MAX, Published, encode_pipes, parse_pipes};
use crazyagent_win_sandbox::{pipe, private};
use std::io::{self, Read, Write};
use std::path::Path;

/// The D7 pipes file's name in its protected root.
const PIPES_FILE: &str = "pipes";

/// Publishes a new start of the endpoint `purpose` in its protected root (DESIGN-0256 D7,
/// leader 07:46): a fresh random part and this engine's pin, staged in a file created private
/// before its first byte, renamed over `pipes` inside the held root, then read back through
/// the private judge. A `pipes` file the judge refuses is left as it is. Inputs: the root and
/// the purpose. Output: what was published, or the error.
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
    let mut file = private::file(&staged, true)?;
    let written = file
        .write_all(encode_pipes(purpose, &published).as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    if let Err(error) = written.and_then(|()| std::fs::rename(&staged, &target)) {
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
