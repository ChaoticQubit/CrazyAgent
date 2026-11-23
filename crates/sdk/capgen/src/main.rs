#![forbid(unsafe_code)]

use std::path::Path;
use std::process::ExitCode;

/// Every mode that generates from a caps folder.
const MODES: [&str; 6] = [
    "--write",
    "--check",
    "--write-ts",
    "--check-ts",
    "--write-wit",
    "--check-wit",
];

/// The usage line for every mode capgen takes.
const USAGE: &str = "usage: capgen --write|--check|--write-ts|--check-ts|--write-wit|--check-wit \
                     <caps dir> <generated dir>, or capgen --test-module <cap file> <out file>";

/// Runs capgen: `--write`/`--check` write or compare the Rust SDK files, `--write-ts`/
/// `--check-ts` the TS ones and `--write-wit`/`--check-wit` the WIT one, from `<caps dir>`
/// into `<generated dir>`; `--test-module <cap file> <out file>` writes one test module.
/// Input: the command line. Output: 0 when written or equal, 1 with each differing path
/// on drift, 2 on a refusal.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [mode, caps, generated] = args.as_slice() else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    if mode == "--test-module" {
        return test_module(Path::new(caps), Path::new(generated));
    }
    if !MODES.contains(&mode.as_str()) {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    let files = match crazyagent_capgen::load(Path::new(caps)).and_then(|loaded| {
        let borrowed: Vec<(&str, crazyagent_cap::File)> = loaded
            .iter()
            .map(|(stem, file)| (stem.as_str(), file.clone()))
            .collect();
        emit(mode, &borrowed)
    }) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{} {}: {}", error.code, error.file, error.detail);
            return ExitCode::from(2);
        }
    };
    let generated = Path::new(generated);
    if mode.starts_with("--write") {
        write(generated, &files)
    } else {
        check(generated, &files)
    }
}

/// Generates the files of one mode's target: Rust for `--write`/`--check`, TS for the
/// `-ts` modes, WIT for the `-wit` modes. Inputs: one of [`MODES`] and the parsed files.
/// Output: the generated files, or the refusal.
fn emit(
    mode: &str,
    files: &[(&str, crazyagent_cap::File)],
) -> Result<Vec<(String, String)>, crazyagent_capgen::CapgenError> {
    match mode {
        "--write" | "--check" => crazyagent_capgen::rust(files),
        "--write-ts" | "--check-ts" => crazyagent_capgen::ts(files),
        "--write-wit" | "--check-wit" => crazyagent_capgen::wit(files),
        _ => unreachable!("{mode} is one of MODES"),
    }
}

/// Writes the generated files into their folder, refusing any other file already there.
/// Inputs: the folder and the files. Output: 0 when written, 2 on an extra file or a
/// write error.
fn write(generated: &Path, files: &[(String, String)]) -> ExitCode {
    if let Err(error) = std::fs::create_dir_all(generated) {
        eprintln!("capgen_write {}: {error}", generated.display());
        return ExitCode::from(2);
    }
    for path in crazyagent_capgen::drift(generated, files) {
        if !files.iter().any(|(name, _)| *name == path) {
            eprintln!("capgen_extra_file {path}");
            return ExitCode::from(2);
        }
    }
    for (path, text) in files {
        if let Err(error) = std::fs::write(generated.join(path), text) {
            eprintln!("capgen_write {path}: {error}");
            return ExitCode::from(2);
        }
    }
    ExitCode::SUCCESS
}

/// Compares the generated files with their folder and prints each differing path.
/// Inputs: the folder and the files. Output: 0 when equal, 1 on drift.
fn check(generated: &Path, files: &[(String, String)]) -> ExitCode {
    let differing = crazyagent_capgen::drift(generated, files);
    for path in &differing {
        println!("{path}");
    }
    if differing.is_empty() {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

/// Writes one `.cap` file as a test module. Inputs: the `.cap` file and the file to write.
/// Output: 0 when written, 2 on a refusal.
fn test_module(cap: &Path, out: &Path) -> ExitCode {
    let Some(stem) = cap.file_stem().and_then(|stem| stem.to_str()) else {
        eprintln!("capgen_file_name {}", cap.display());
        return ExitCode::from(2);
    };
    let written = std::fs::read_to_string(cap)
        .map_err(|error| format!("capgen_read {}: {error}", cap.display()))
        .and_then(|source| {
            crazyagent_cap::parse_as(stem, &source).map_err(|error| {
                format!(
                    "{} {}: line {} column {}",
                    error.code,
                    cap.display(),
                    error.line,
                    error.column
                )
            })
        })
        .and_then(|file| {
            crazyagent_capgen::rust_test_module(stem, &file)
                .map_err(|error| format!("{} {}: {}", error.code, error.file, error.detail))
        })
        .and_then(|text| {
            std::fs::write(out, text)
                .map_err(|error| format!("capgen_write {}: {error}", out.display()))
        });
    match written {
        Ok(()) => ExitCode::SUCCESS,
        Err(line) => {
            eprintln!("{line}");
            ExitCode::from(2)
        }
    }
}
