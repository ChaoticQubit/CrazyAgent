#![forbid(unsafe_code)]

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{ErrorKind, Write};
use std::path::Path;
use std::process::{Command, Stdio};

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

/// Lays one generated file out with `RUSTFMT` when set, else the toolchain's `rustfmt`
/// on the path. Inputs: the file's path, for refusals, and its text. Output: as
/// [`rustfmt_with`].
fn rustfmt(path: &str, text: &str) -> Result<String, CapgenError> {
    let program = std::env::var_os("RUSTFMT").unwrap_or_else(|| OsString::from("rustfmt"));
    rustfmt_with(&program, path, text)
}

/// Lays one generated file out with a rustfmt program, edition 2024, text in on stdin and
/// out on stdout. Inputs: the program, the file's path, for refusals, and its text.
/// Output: the formatted text, or `capgen_rustfmt_missing` (no such program) or
/// `capgen_rustfmt_refused`, never the raw text.
fn rustfmt_with(program: &OsStr, path: &str, text: &str) -> Result<String, CapgenError> {
    let refuse = |code: &'static str, detail: String| CapgenError {
        file: path.to_owned(),
        code,
        detail,
    };
    let mut child = Command::new(program)
        .args(["--edition", "2024", "--emit", "stdout"])
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
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let input = text.to_owned();
    let writer = std::thread::spawn(move || stdin.write_all(input.as_bytes()));
    let output = child
        .wait_with_output()
        .map_err(|error| refuse("capgen_rustfmt_refused", error.to_string()))?;
    let written = writer.join().expect("the stdin writer does not panic");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let first = stderr.lines().next().unwrap_or("").to_owned();
        return Err(refuse(
            "capgen_rustfmt_refused",
            format!("{}: {first}", output.status),
        ));
    }
    written.map_err(|error| refuse("capgen_rustfmt_refused", error.to_string()))?;
    let formatted = String::from_utf8(output.stdout)
        .map_err(|_| refuse("capgen_rustfmt_refused", "output is not UTF-8".to_owned()))?;
    assert!(
        !formatted.is_empty() || text.trim().is_empty(),
        "rustfmt kept the file"
    );
    Ok(formatted)
}

/// Reads and parses every `.cap` file of a folder. Input: the folder.
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
        let source = fs::read_to_string(caps.join(&name))
            .map_err(|error| refuse(&name, "capgen_read", error.to_string()))?;
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

/// Compares generated files with a folder's files, the folder's subfolders read one level
/// deep with paths `<subfolder>/<file>`. Inputs: the folder and the generated files.
/// Output: every path that differs, is missing or is extra in the folder, sorted.
pub fn drift(generated: &Path, files: &[(String, String)]) -> Vec<String> {
    let mut differing = Vec::new();
    for (path, text) in files {
        if fs::read_to_string(generated.join(path)).ok().as_deref() != Some(text.as_str()) {
            differing.push(path.clone());
        }
    }
    let mut present = Vec::new();
    for (name, is_folder) in entries(generated) {
        if !is_folder {
            present.push(name);
            continue;
        }
        let inner = entries(&generated.join(&name));
        for (file, _) in &inner {
            present.push(format!("{name}/{file}"));
        }
        if inner.is_empty() {
            present.push(name);
        }
    }
    for name in present {
        if !files.iter().any(|(path, _)| *path == name) {
            differing.push(name);
        }
    }
    differing.sort();
    assert!(differing.windows(2).all(|pair| pair[0] != pair[1]));
    differing
}

/// Lists one folder's entries. Input: the folder. Output: each entry's name with whether
/// it is a folder, empty when the folder does not read.
fn entries(folder: &Path) -> Vec<(String, bool)> {
    let Ok(read) = fs::read_dir(folder) else {
        return Vec::new();
    };
    read.flatten()
        .map(|entry| {
            let is_folder = entry.file_type().is_ok_and(|kind| kind.is_dir());
            (entry.file_name().to_string_lossy().into_owned(), is_folder)
        })
        .collect()
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
