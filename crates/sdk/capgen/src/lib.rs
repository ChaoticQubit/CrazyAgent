#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{ErrorKind, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crazyagent_cap::File;

/// The most `.cap` files one SDK may hold.
pub const CAP_FILES_MAX: usize = 64;

/// Why capgen refused: the `.cap` file, the refusal code and what was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapgenError {
    pub file: String,
    pub code: &'static str,
    pub detail: String,
}

/// Writes the Rust SDK for the parsed `.cap` files, deterministic for the same input, each
/// file laid out by the toolchain's own rustfmt so `cargo fmt --check` holds at any size.
/// Input: each file's stem (its module name) with its parsed file, any order.
/// Output: each generated file's path below `src/generated` with its text, sorted by path,
/// or the first refusal (`capgen_reserved_name`, `capgen_rustfmt_missing`, ...).
pub fn rust(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    rust::files(files)?
        .into_iter()
        .map(|(path, text)| {
            let formatted = rustfmt(&path, &text)?;
            Ok((path, formatted))
        })
        .collect()
}

/// Writes the TypeScript SDK for the parsed `.cap` files after every check the Rust SDK
/// runs, laid out by capgen itself, byte-stable for the same input in any order.
/// Input: each file's stem with its parsed file, any order.
/// Output: `index.ts` and one `<stem>.ts` per file with their texts, sorted by path, or the
/// first refusal (`capgen_reserved_name` with the detail `ts: <name>`, ...).
pub fn ts(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    ts::files(files)
}

/// Writes the WIT SDK, one `sdk.wit` of package `crazyagent:sdk`, after every check the Rust
/// SDK runs. A handle is `type h = u64`, not a `resource`, which in a record would move the
/// handle away on every call; the kernel checks every handle against its caller, so a
/// forged id gets nothing. Input: stems with parsed files, any order. Output: the one
/// file, or the first refusal (`capgen_wit_name`, `capgen_wit_cycle`, ...).
pub fn wit(files: &[(&str, File)]) -> Result<Vec<(String, String)>, CapgenError> {
    wit::files(files)
}

/// Writes one `.cap` file as a single test module with its own `Host` trait, laid out by
/// rustfmt, for the SDK's tests to compile every emitter. Inputs: the stem and the parsed
/// file. Output: the module text, or the first refusal.
pub fn rust_test_module(stem: &str, file: &File) -> Result<String, CapgenError> {
    let text = rust::test_module(stem, file)?;
    rustfmt(&format!("{stem}.rs"), &text)
}

/// How long capgen waits for one rustfmt run before it stops the process.
pub(crate) const RUSTFMT_DEADLINE: Duration = Duration::from_secs(30);

/// How long capgen sleeps between two checks of a running rustfmt.
const RUSTFMT_POLL: Duration = Duration::from_millis(2);

/// The empty rustfmt config every run uses, checked in beside capgen, so a `rustfmt.toml`
/// in the caller's working folder never changes the generated layout. Output: its path.
pub(crate) fn rustfmt_config() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("rustfmt.toml")
}

/// Lays one generated file out with `RUSTFMT` when set, else the toolchain's `rustfmt`
/// on the path, with capgen's empty config and a 30 s bound. Inputs: the file's path, for
/// refusals, and its text. Output: as [`rustfmt_with`].
fn rustfmt(path: &str, text: &str) -> Result<String, CapgenError> {
    let program = std::env::var_os("RUSTFMT").unwrap_or_else(|| OsString::from("rustfmt"));
    rustfmt_with(&program, &rustfmt_config(), path, text, RUSTFMT_DEADLINE)
}

/// Lays one generated file out with a rustfmt program, edition 2024 and `--config-path`,
/// text in on stdin and out on stdout, stopping it past the deadline. Inputs: the program,
/// the config, the file's path, for refusals, its text and the deadline. Output: the
/// formatted text, or `capgen_rustfmt_missing` (no program or config),
/// `capgen_rustfmt_timeout` or `capgen_rustfmt_refused`, never the raw text.
pub(crate) fn rustfmt_with(
    program: &OsStr,
    config: &Path,
    path: &str,
    text: &str,
    deadline: Duration,
) -> Result<String, CapgenError> {
    let refuse = |code: &'static str, detail: String| CapgenError {
        file: path.to_owned(),
        code,
        detail,
    };
    if !config.is_file() {
        return Err(refuse(
            "capgen_rustfmt_missing",
            format!("{} is missing", config.display()),
        ));
    }
    let mut child = Command::new(program)
        .args(["--edition", "2024", "--emit", "stdout", "--config-path"])
        .arg(config)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            let detail = if error.kind() == ErrorKind::NotFound {
                format!(
                    "{} not found; install the toolchain component with `rustup component add rustfmt` (the rust:1.98.1-slim image ships without it)",
                    program.to_string_lossy()
                )
            } else {
                format!("{}: {error}", program.to_string_lossy())
            };
            refuse("capgen_rustfmt_missing", detail)
        })?;
    let pipes = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let (Some(mut stdin), Some(stdout), Some(stderr)) = pipes else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(refuse(
            "capgen_rustfmt_refused",
            "a pipe is missing".to_owned(),
        ));
    };
    let input = text.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let out = drain(stdout);
    let err = drain(stderr);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(refuse(
                    "capgen_rustfmt_timeout",
                    format!("no answer within {} ms", deadline.as_millis()),
                ));
            }
            Ok(None) => std::thread::sleep(RUSTFMT_POLL),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(refuse("capgen_rustfmt_refused", error.to_string()));
            }
        }
    };
    let joined = |name: &str| {
        refuse(
            "capgen_rustfmt_refused",
            format!("the {name} thread panicked"),
        )
    };
    let written = writer.join().map_err(|_| joined("stdin"))?;
    let stdout = out.join().map_err(|_| joined("stdout"))?;
    let stderr = err.join().map_err(|_| joined("stderr"))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr.unwrap_or_default()).into_owned();
        let first = stderr.lines().next().unwrap_or("").to_owned();
        return Err(refuse(
            "capgen_rustfmt_refused",
            format!("{status}: {first}"),
        ));
    }
    let stdout = stdout.map_err(|error| refuse("capgen_rustfmt_refused", error.to_string()))?;
    if stdout.is_empty() && !text.trim().is_empty() {
        return Err(refuse("capgen_rustfmt_refused", "empty output".to_owned()));
    }
    written.map_err(|error| refuse("capgen_rustfmt_refused", error.to_string()))?;
    String::from_utf8(stdout)
        .map_err(|_| refuse("capgen_rustfmt_refused", "output is not UTF-8".to_owned()))
}

/// Reads one child pipe to its end on its own thread, so neither pipe can fill and stall
/// the child while capgen waits. Input: the pipe. Output: the thread, which gives the bytes.
fn drain<R: Read + Send + 'static>(
    mut pipe: R,
) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        pipe.read_to_end(&mut bytes).map(|_| bytes)
    })
}

/// Reads and parses every `.cap` file of a folder. Each must be a regular file, never a
/// symlink, FIFO or folder (`capgen_file_kind`), of at most `SOURCE_BYTES_MAX` bytes by its
/// metadata and by a bounded read (`cap_source_too_large`). Input: the folder.
/// Output: each file's stem with its parsed file, sorted by stem, or the first refusal.
pub fn load(caps: &Path) -> Result<Vec<(String, File)>, CapgenError> {
    let refuse = |file: &str, code: &'static str, detail: String| CapgenError {
        file: file.to_owned(),
        code,
        detail,
    };
    let entries = fs::read_dir(caps).map_err(|error| {
        refuse(
            &caps.display().to_string(),
            "capgen_read",
            error.to_string(),
        )
    })?;
    let mut stems = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| refuse("", "capgen_read", error.to_string()))?
            .path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("cap") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
            return Err(refuse(
                &path.display().to_string(),
                "capgen_file_name",
                String::new(),
            ));
        };
        stems.push(stem.to_owned());
    }
    stems.sort();
    if stems.len() > CAP_FILES_MAX {
        return Err(refuse("", "capgen_too_many_files", stems.len().to_string()));
    }
    let mut loaded = Vec::with_capacity(stems.len());
    for stem in stems {
        let name = format!("{stem}.cap");
        let source = read_bounded(&caps.join(&name))
            .map_err(|(code, detail)| refuse(&name, code, detail))?;
        let file = crazyagent_cap::parse_as(&stem, &source).map_err(|error| {
            refuse(
                &name,
                error.code,
                format!("line {} column {}", error.line, error.column),
            )
        })?;
        loaded.push((stem, file));
    }
    Ok(loaded)
}

/// Reads one `.cap` file without following a link: a regular file only, refused by its
/// metadata length past `SOURCE_BYTES_MAX` before any byte is read, then read through a
/// bound one byte past it, so a file that grows between the two steps is still refused.
/// Input: the path. Output: the UTF-8 source, or the refusal code with its detail.
fn read_bounded(path: &Path) -> Result<String, (&'static str, String)> {
    let max = crazyagent_cap::SOURCE_BYTES_MAX;
    let too_large = || ("cap_source_too_large", "line 1 column 1".to_owned());
    let meta = fs::symlink_metadata(path).map_err(|error| ("capgen_read", error.to_string()))?;
    let kind = meta.file_type();
    if !kind.is_file() {
        let what = if kind.is_symlink() {
            "a symlink"
        } else if kind.is_dir() {
            "a folder"
        } else {
            "not a regular file"
        };
        return Err(("capgen_file_kind", what.to_owned()));
    }
    if meta.len() > max as u64 {
        return Err(too_large());
    }
    let file = fs::File::open(path).map_err(|error| ("capgen_read", error.to_string()))?;
    let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(max));
    file.take(max as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| ("capgen_read", error.to_string()))?;
    if bytes.len() > max {
        return Err(too_large());
    }
    String::from_utf8(bytes).map_err(|error| ("capgen_read", error.to_string()))
}

/// Compares generated files with a folder's files, the folder's subfolders read one level
/// deep with paths `<subfolder>/<file>`. A name that is not UTF-8 and a folder or entry
/// that cannot be read are drift of their own, never skipped; an empty subfolder is extra
/// only when no generated path lies below it. Inputs: the folder and the
/// generated files. Output: every path that differs, is missing or is extra, sorted, once.
pub fn drift(generated: &Path, files: &[(String, String)]) -> Vec<String> {
    let mut differing = Vec::new();
    for (path, text) in files {
        if fs::read_to_string(generated.join(path)).ok().as_deref() != Some(text.as_str()) {
            differing.push(path.clone());
        }
    }
    let mut present = Vec::new();
    for entry in entries(generated, ".") {
        let Entry::Folder(name) = entry else {
            present.push(entry.into_name());
            continue;
        };
        let inner = entries(&generated.join(&name), &name);
        for entry in &inner {
            present.push(match entry {
                Entry::Marked(marked) => marked.clone(),
                Entry::File(file) | Entry::Folder(file) => format!("{name}/{file}"),
            });
        }
        let expected = files.iter().any(|(path, _)| {
            path.strip_prefix(&name)
                .is_some_and(|rest| rest.starts_with('/'))
        });
        if inner.is_empty() && !expected {
            present.push(name);
        }
    }
    for name in present {
        if !files.iter().any(|(path, _)| *path == name) {
            differing.push(name);
        }
    }
    differing.sort();
    differing.dedup();
    assert!(differing.windows(2).all(|pair| pair[0] < pair[1]));
    differing
}

/// One entry of a folder `drift` reads: a file, a folder, or a marked name that is drift
/// by itself (not UTF-8, or unreadable), already in its reported form.
enum Entry {
    File(String),
    Folder(String),
    Marked(String),
}

impl Entry {
    /// The entry's name as `drift` reports it. Output: the name.
    fn into_name(self) -> String {
        match self {
            Self::File(name) | Self::Folder(name) | Self::Marked(name) => name,
        }
    }
}

/// Names one folder entry. Input: the raw name. Output: the name, or its lossy text with
/// ` (not utf-8)` when it is not UTF-8.
pub(crate) fn entry_name(name: &OsStr) -> Result<String, String> {
    name.to_str()
        .map(str::to_owned)
        .ok_or_else(|| format!("{} (not utf-8)", name.to_string_lossy()))
}

/// Lists one folder's entries. A missing folder has none; a folder or entry that cannot be
/// read is one `<label> (unreadable)` entry, and a marked name is reported below the label.
/// Inputs: the folder and its label in reports, `.` for the top folder. Output: the entries.
fn entries(folder: &Path, label: &str) -> Vec<Entry> {
    let unreadable = || Entry::Marked(format!("{label} (unreadable)"));
    let below = |name: String| {
        if label == "." {
            name
        } else {
            format!("{label}/{name}")
        }
    };
    let read = match fs::read_dir(folder) {
        Ok(read) => read,
        Err(error) if error.kind() == ErrorKind::NotFound => return Vec::new(),
        Err(_) => return vec![unreadable()],
    };
    let mut out = Vec::new();
    for entry in read {
        let Ok(entry) = entry else {
            out.push(unreadable());
            continue;
        };
        let name = match entry_name(&entry.file_name()) {
            Ok(name) => name,
            Err(marked) => {
                out.push(Entry::Marked(below(marked)));
                continue;
            }
        };
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => out.push(Entry::Folder(name)),
            Ok(_) => out.push(Entry::File(name)),
            Err(_) => out.push(Entry::Marked(below(format!("{name} (unreadable)")))),
        }
    }
    out
}

#[cfg(test)]
mod events_tests;
mod rust;
mod rust_wire;
#[cfg(test)]
mod tests;
mod ts;
#[cfg(test)]
mod ts_tests;
#[cfg(test)]
mod wire_tests;
mod wit;
#[cfg(test)]
mod wit_tests;
