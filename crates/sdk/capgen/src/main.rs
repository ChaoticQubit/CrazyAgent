#![forbid(unsafe_code)]

use std::path::Path;
use std::process::ExitCode;

/// Runs capgen: `--write <caps dir> <generated dir>` writes the Rust SDK files there,
/// `--check <caps dir> <generated dir>` compares them, and `--test-module <cap file>
/// <out file>` writes one file as a test module with its own `Host` trait.
/// Input: the command line. Output: 0 when written or equal, 1 with each differing path
/// on drift, 2 on a refusal.
fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [mode, caps, generated] = args.as_slice() else {
        eprintln!("usage: capgen --write|--check <caps dir> <generated dir>");
        return ExitCode::from(2);
    };
    if mode == "--test-module" {
        return test_module(Path::new(caps), Path::new(generated));
    }
    let generated = Path::new(generated);
    let files = match crazyagent_capgen::load(Path::new(caps)).and_then(|loaded| {
        let borrowed: Vec<(&str, crazyagent_cap::File)> = loaded
            .iter()
            .map(|(stem, file)| (stem.as_str(), file.clone()))
            .collect();
        crazyagent_capgen::rust(&borrowed)
    }) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("{} {}: {}", error.code, error.file, error.detail);
            return ExitCode::from(2);
        }
    };
    match mode.as_str() {
        "--write" => {
            if let Err(error) = std::fs::create_dir_all(generated) {
                eprintln!("capgen_write {}: {error}", generated.display());
                return ExitCode::from(2);
            }
            for path in crazyagent_capgen::drift(generated, &files) {
                if !files.iter().any(|(name, _)| *name == path) {
                    eprintln!("capgen_extra_file {path}");
                    return ExitCode::from(2);
                }
            }
            for (path, text) in &files {
                if let Err(error) = std::fs::write(generated.join(path), text) {
                    eprintln!("capgen_write {path}: {error}");
                    return ExitCode::from(2);
                }
            }
            ExitCode::SUCCESS
        }
        "--check" => {
            let differing = crazyagent_capgen::drift(generated, &files);
            for path in &differing {
                println!("{path}");
            }
            if differing.is_empty() {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            }
        }
        _ => {
            eprintln!("usage: capgen --write|--check <caps dir> <generated dir>");
            ExitCode::from(2)
        }
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
